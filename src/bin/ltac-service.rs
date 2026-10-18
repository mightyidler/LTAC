#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    if (if std::env::args().nth(1).as_deref() == Some("--configure-service") {
        ltac::service_ipc::configure_preshutdown()
    } else {
        ltac::service::dispatch()
    })
    .is_err()
    {
        std::process::exit(1);
    }
    #[cfg(not(windows))]
    {
        eprintln!("LTAC service requires Windows");
        std::process::exit(1);
    }
}
