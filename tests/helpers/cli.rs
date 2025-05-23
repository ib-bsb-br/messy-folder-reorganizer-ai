use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;

use messy_folder_reorganizer_ai::configuration::consts::MESSY_FOLDER_REORGANIZER_AI_PATH;

pub fn run_reorganization(
    path_to_app_folder: &str,
    source: &str,
    destination: &str,
    embeddings_model_name: &str,
    llm_model_name: &str,
    llm_address: &str,
    qdrant_address: &str,
    mode: &OutputMode,
) -> std::io::Result<Option<String>> {
    let args = [
        "process",
        "--source",
        source,
        "--destination",
        destination,
        "-L",
        llm_model_name,
        "-E",
        embeddings_model_name,
        "-n",
        llm_address,
        "-q",
        qdrant_address,
        "-F",
        "-R",
    ];

    run_command_realtime(path_to_app_folder, PATH_TO_BINARY, &args, mode)
}

pub fn run_rollback(
    path_to_app_folder: &str,
    mode: &OutputMode,
    session_id: &str,
) -> std::io::Result<Option<String>> {
    let args = ["rollback", "-i", session_id];
    run_command_realtime(path_to_app_folder, PATH_TO_BINARY, &args, mode)
}

pub fn run_apply(
    path_to_app_folder: &str,
    mode: &OutputMode,
    session_id: &str,
) -> std::io::Result<Option<String>> {
    let args = ["apply", "-i", session_id];
    run_command_realtime(path_to_app_folder, PATH_TO_BINARY, &args, mode)
}

const PATH_TO_BINARY: &str = "./target/debug/messy_folder_reorganizer_ai";

pub enum OutputMode {
    ToFile(String), // we always need to log to file for capturing session_id from logs
}

fn setup_command_and_logging(
    path_to_app_folder: &str,
    program: &str,
    args: &[&str],
    output: &OutputMode,
) -> std::io::Result<(Command, Option<File>)> {
    let mut command = Command::new(program);
    command.args(args);
    command
        .env("RUST_BACKTRACE", "1")
        .env(MESSY_FOLDER_REORGANIZER_AI_PATH, path_to_app_folder);

    let (stdout, stderr, log_file): (Stdio, Stdio, Option<File>) = match output {
        OutputMode::ToFile(path) => {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)?;
            let file_for_stderr = file.try_clone()?; // for separate use in stderr thread
            (Stdio::piped(), Stdio::from(file_for_stderr), Some(file))
        }
    };

    command.stdout(stdout).stderr(stderr);
    Ok((command, log_file))
}

fn spawn_output_thread<R: std::io::Read + Send + 'static>(
    stream: R,
    label: &'static str,
    mut output: OutputTarget,
    session_tx: Option<mpsc::Sender<String>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines().map_while(Result::ok) {
            let msg = format!("[{}] {}\n", label, line);
            output.write(&msg);

            if let Some(tx) = &session_tx {
                if line.contains("Session id: ") {
                    if let Some(id) = line.split("Session id: ").nth(1) {
                        // string should be sa,e as in messages.rs
                        tx.send(id.trim().to_string()).ok();
                    }
                }
            }
        }
    })
}

enum OutputTarget {
    ConsoleStdout,
    File(File),
}

impl OutputTarget {
    fn write(&mut self, line: &str) {
        match self {
            OutputTarget::ConsoleStdout => print!("{}", line),
            OutputTarget::File(file) => {
                let _ = file.write_all(line.as_bytes());
            }
        }
    }
}

pub fn run_command_realtime(
    path_to_app_folder: &str,
    program: &str,
    args: &[&str],
    output: &OutputMode,
) -> std::io::Result<Option<String>> {
    let (mut command, log_file) =
        setup_command_and_logging(path_to_app_folder, program, args, output)?;

    assert!(
        std::path::Path::new(PATH_TO_BINARY).exists(),
        "Binary not built. Run `cargo build` first."
    );

    let mut child = command.spawn()?;

    let mut stdout_thread = None;
    let mut stderr_thread = None;

    let (session_tx, session_rx) = mpsc::channel();

    if let Some(stdout) = child.stdout.take() {
        let log_for_stdout = match &output {
            OutputMode::ToFile(_) => Some(log_file.as_ref().unwrap().try_clone()?),
        };

        let target = match log_for_stdout {
            Some(file) => OutputTarget::File(file),
            None => OutputTarget::ConsoleStdout,
        };

        stdout_thread = Some(spawn_output_thread(
            stdout,
            "stdout",
            target,
            Some(session_tx),
        ));
    }

    if let Some(stderr) = child.stderr.take() {
        let target = match &output {
            OutputMode::ToFile(_) => OutputTarget::File(log_file.unwrap()),
        };

        stderr_thread = Some(spawn_output_thread(stderr, "stderr", target, None));
    }

    let status = child.wait()?;

    if let Some(handle) = stdout_thread {
        handle.join().unwrap();
    }

    if let Some(handle) = stderr_thread {
        handle.join().unwrap();
    }

    if !status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("CLI execution exited with status: {}", status),
        ));
    }

    let session_id = session_rx.try_recv().ok();
    Ok(session_id)
}
