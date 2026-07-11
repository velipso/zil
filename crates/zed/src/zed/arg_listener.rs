use futures::channel::mpsc;
use serde::{Serialize, Deserialize};
use std::{
    fs::{File, OpenOptions, remove_file},
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::Path,
};
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub(crate) struct ArgListenerCommand {
    pub cwd: String,
    pub args: Vec<String>,
}

pub(crate) enum ArgListenerResult {
    Exit,
    Create(mpsc::UnboundedReceiver<ArgListenerCommand>),
}

#[derive(Serialize, Deserialize, Debug)]
struct LockFileData {
    pid: u32,
    port: u16,
    token: String,
}

#[derive(Serialize, Deserialize, Debug)]
enum LocalMessage {
    Hello(String),
    Command(ArgListenerCommand),
}

#[derive(Debug)]
struct UseError {
    retry: bool,
    err: String,
}

fn read_stream(
    stream: TcpStream,
    token: &str,
    tx: mpsc::UnboundedSender<ArgListenerCommand>
) -> Result<(), String> {
    let reader = BufReader::new(stream);
    let mut authenticated = false;

    for line in reader.lines() {
        let line: String = line
            .map_err(|err| format!("Failed to read socket: {err}"))?;
        let message: LocalMessage = serde_json::from_str(line.as_str())
            .map_err(|err| format!("Invalid local message: {err}\n{line}"))?;

        match message {
            LocalMessage::Hello(sent_token) => {
                if sent_token != token {
                    return Err("Invalid token".to_string());
                }
                authenticated = true;
            },
            LocalMessage::Command(command) => {
                if !authenticated {
                    return Err("Command received before authentication".to_string());
                }
                tx.unbounded_send(command).unwrap();
            },
        }
    }

    Ok(())
}

fn create_lock_file(
    token: &str,
    lock_file_path: &Path,
    mut lock_file: File,
    args: &[String],
) -> Result<ArgListenerResult, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|err| format!("Failed to bind to local address: {err}"))?;
    let port = listener.local_addr().map(|addr| addr.port())
        .map_err(|err| format!("Failed to get port: {err}"))?;
    let lock_file_data = LockFileData {
        pid: std::process::id(),
        port,
        token: token.to_string(),
    };
    let output = format!(
        "{}\n{}\n{}",
        "// This file is used to ensure only one copy of Zil is running",
        "// It should automatically be deleted when Zil exits",
        serde_json::to_string_pretty(&lock_file_data).unwrap()
    );
    lock_file.write_all(output.as_bytes())
        .map_err(|err| {
            format!(
                "Failed to write to lock file {}: {}",
                lock_file_path.display(),
                err
            )
        })?;
    drop(lock_file);

    let (tx, rx) = mpsc::unbounded::<ArgListenerCommand>();

    let cwd = std::env::current_dir()
        .map_err(|err| format!("Failed to get cwd: {err}"))?
        .to_string_lossy()
        .to_string();
    tx.unbounded_send(ArgListenerCommand { cwd, args: args.to_vec() }).unwrap();

    let token = token.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    if let Err(err) = read_stream(stream, token.as_str(), tx.clone()) {
                        eprintln!("{err}");
                    }
                }
                Err(err) => {
                    eprintln!("Connection failed: {err}");
                }
            }
        }
    });

    Ok(ArgListenerResult::Create(rx))
}

fn use_lock_file(
    lock_file_path: &Path,
    args: &[String],
) -> Result<ArgListenerResult, UseError> {
    // read the lock file
    let lock_file_str = std::fs::read_to_string(&lock_file_path)
        .map_err(|err| UseError {
            retry: true,
            err: format!("Cannot read: {err}"),
        })?;
    let lock_file_data: LockFileData = serde_json_lenient::from_str(lock_file_str.as_str())
        .map_err(|err| UseError {
            retry: true,
            err: format!("Cannot parse: {err}"),
        })?;

    // connect to the port
    let addr = format!("127.0.0.1:{}", lock_file_data.port);
    let mut stream = TcpStream::connect(addr)
        .map_err(|err| UseError {
            retry: true,
            err: format!("Failed to connect to already running zil: {err}"),
        })?;

    // send hello
    let hello = LocalMessage::Hello(lock_file_data.token);
    writeln!(stream, "{}", serde_json::to_string(&hello).unwrap())
        .map_err(|err| UseError {
            retry: false,
            err: format!("Failed to write hello to TCP socket: {err}"),
        })?;

    // send arguments
    let cwd = std::env::current_dir()
        .map_err(|err| UseError {
            retry: false,
            err: format!("Failed to get cwd: {err}")
        })?
        .to_string_lossy()
        .to_string();
    let command = LocalMessage::Command(ArgListenerCommand {
        cwd,
        args: args.to_vec()
    });
    writeln!(stream, "{}", serde_json::to_string(&command).unwrap())
        .map_err(|err| UseError {
            retry: false,
            err: format!("Failed to write command to TCP socket: {err}"),
        })?;

    stream.flush().map_err(|err| UseError {
        retry: false,
        err: format!("Failed to flush TCP socket: {err}"),
    })?;

    Ok(ArgListenerResult::Exit)
}

pub(crate) fn handle_args(args: &[String]) -> Result<ArgListenerResult, String> {
    let token = Uuid::new_v4().to_string();
    let lock_file_path = paths::config_dir().join("lock.json");

    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_file_path)
    {
        Ok(lock_file) => create_lock_file(
            token.as_str(),
            &lock_file_path,
            lock_file,
            args,
        ),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            use_lock_file(&lock_file_path, args)
                .or_else(|err| {
                    if err.retry {
                        if let Err(file_err) = remove_file(&lock_file_path) {
                            Err(format!(
                                "Lock file is invalid ({}), and cannot remove {}: {}",
                                err.err,
                                lock_file_path.display(),
                                file_err
                            ))
                        } else {
                            match OpenOptions::new()
                                .write(true)
                                .create_new(true)
                                .open(&lock_file_path)
                            {
                                Ok(lock_file) => create_lock_file(
                                    token.as_str(),
                                    &lock_file_path,
                                    lock_file,
                                    args,
                                ),
                                Err(create_err) => Err(format!(
                                "Lock file is invalid ({}), and failed to recreate it {}: {}",
                                    err.err,
                                    lock_file_path.display(),
                                    create_err
                                )),
                            }
                        }
                    } else {
                        Err(err.err)
                    }
                })
        }
        Err(err) => Err(format!("Failed to create lock file: {err}")),
    }
}

pub(crate) fn handle_args_exit() {
    let path = paths::config_dir().join("lock.json");
    remove_file(&path).ok();
}
