use serde::Deserialize;
use std::borrow::Cow;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::bot::Bot;
use crate::protocol::{stat, Hello, Observation, Ready};

#[derive(Deserialize)]
struct MsgKind<'a> {
    #[serde(rename = "type", borrow)]
    kind: Cow<'a, str>,
}

#[derive(Default)]
struct Inbox {
    hello: Option<String>,
    latest_obs: Option<String>,
    ended: bool,
    eof: bool,
}

pub fn run() -> io::Result<()> {
    let shared = Arc::new((Mutex::new(Inbox::default()), Condvar::new()));
    let reader_shared = Arc::clone(&shared);

    thread::Builder::new()
        .name("stdin-reader".into())
        .spawn(move || {
            let stdin = io::stdin();
            let mut reader = BufReader::with_capacity(256 * 1024, stdin.lock());
            let (lock, cvar) = &*reader_shared;

            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        let mut inbox = lock.lock().unwrap();
                        inbox.eof = true;
                        cvar.notify_one();
                        break;
                    }
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Ok(msg) = serde_json::from_str::<MsgKind<'_>>(trimmed) {
                            let mut inbox = lock.lock().unwrap();
                            match msg.kind.as_ref() {
                                "hello" => inbox.hello = Some(line),
                                "obs" => inbox.latest_obs = Some(line),
                                "end" => {
                                    inbox.ended = true;
                                    cvar.notify_one();
                                    break;
                                }
                                _ => {}
                            }
                            // Drain any additional complete lines already buffered by BufReader
                            // before waking the main thread so queued obs batches collapse atomically.
                            while reader.buffer().contains(&b'\n') {
                                let mut next = String::new();
                                match reader.read_line(&mut next) {
                                    Ok(0) => {
                                        inbox.eof = true;
                                        cvar.notify_one();
                                        return;
                                    }
                                    Ok(_) => {
                                        let next_trimmed = next.trim();
                                        if next_trimmed.is_empty() {
                                            continue;
                                        }
                                        if let Ok(next_msg) =
                                            serde_json::from_str::<MsgKind<'_>>(next_trimmed)
                                        {
                                            match next_msg.kind.as_ref() {
                                                "hello" => inbox.hello = Some(next),
                                                "obs" => inbox.latest_obs = Some(next),
                                                "end" => {
                                                    inbox.ended = true;
                                                    cvar.notify_one();
                                                    return;
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("stdin read error: {e}");
                                        inbox.eof = true;
                                        cvar.notify_one();
                                        return;
                                    }
                                }
                            }
                            cvar.notify_one();
                        }
                    }
                    Err(e) => {
                        eprintln!("stdin read error: {e}");
                        let mut inbox = lock.lock().unwrap();
                        inbox.eof = true;
                        cvar.notify_one();
                        break;
                    }
                }
            }
        })?;

    let stdout = io::stdout();
    let mut out = BufWriter::with_capacity(64 * 1024, stdout.lock());
    let (lock, cvar) = &*shared;

    let hello_line = {
        let mut inbox = lock.lock().unwrap();
        while inbox.hello.is_none() && !inbox.ended && !inbox.eof {
            inbox = cvar.wait(inbox).unwrap();
        }
        if inbox.ended || inbox.hello.is_none() {
            return Ok(());
        }
        inbox.hello.take().unwrap()
    };

    let hello: Hello = serde_json::from_str(hello_line.trim())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("invalid hello: {e}")))?;

    let max_line_bytes = stat(&hello.config.bot, "max_line_bytes", 65536.0) as usize;
    let min_pitch = stat(&hello.config.mech, "min_pitch", -0.7);
    let max_pitch = stat(&hello.config.mech, "max_pitch", 1.3);

    let mut bot = Bot::new(hello);

    let ready = Ready {
        msg_type: "ready",
        name: bot.name(),
    };
    serde_json::to_writer(&mut out, &ready)?;
    out.write_all(b"\n")?;
    out.flush()?;

    let mut json_buf: Vec<u8> = Vec::with_capacity(4096);

    loop {
        let obs_line = {
            let mut inbox = lock.lock().unwrap();
            while inbox.latest_obs.is_none() && !inbox.ended && !inbox.eof {
                inbox = cvar.wait(inbox).unwrap();
            }
            if inbox.ended {
                break;
            }
            match inbox.latest_obs.take() {
                Some(line) => line,
                None => break,
            }
        };

        let obs: Observation = match serde_json::from_str(obs_line.trim()) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("failed to parse obs: {e}");
                continue;
            }
        };

        let mut action = bot.act(&obs);
        action.tick = obs.tick;
        action.sanitize(min_pitch, max_pitch, obs.self_obs.aim);

        json_buf.clear();
        if let Err(e) = serde_json::to_writer(&mut json_buf, &action) {
            eprintln!("failed to serialize action at tick {}: {e}", obs.tick);
            json_buf.clear();
            let _ = serde_json::to_writer(&mut json_buf, &serde_json::json!({ "tick": obs.tick }));
        }

        if json_buf.len() >= max_line_bytes {
            action.debug = None;
            json_buf.clear();
            let _ = serde_json::to_writer(&mut json_buf, &action);
            if json_buf.len() >= max_line_bytes {
                json_buf.clear();
                let _ =
                    serde_json::to_writer(&mut json_buf, &serde_json::json!({ "tick": obs.tick }));
            }
        }

        out.write_all(&json_buf)?;
        out.write_all(b"\n")?;
        out.flush()?;
    }

    Ok(())
}
