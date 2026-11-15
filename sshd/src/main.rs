//! kindil-sshd: kindil's SSH server on the Kindle (see lib.rs), run and
//! restarted by kindil (src/ssh.rs).
use std::process::ExitCode;

fn main() -> ExitCode {
    let args = match sshd::Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("kindil-sshd: {error}");
            eprintln!("usage: kindil-sshd [--port N] [--dir DIR] [--listen ADDR]");
            return ExitCode::from(2);
        }
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let result = runtime.and_then(|runtime| {
        runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind((args.listen, args.port)).await?;
            sshd::serve(listener, sshd::Settings::new(&args.dir)).await
        })
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("kindil-sshd: {error}");
            ExitCode::FAILURE
        }
    }
}
