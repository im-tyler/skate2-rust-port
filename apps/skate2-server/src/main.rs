//! The dedicated server alone: `skate2-pc --server` without the game, for a host with no display
//! (a Linux VPS). It takes the game builds of its own release (`../build_secret.rs`).
//!
//! `skate2-server [ip:port | ip | :port]`, by default every address at port 32082. A release build
//! says when a newer release is out, and `skate2-server --update` puts the newest release's Linux
//! zip in place of this one (`docs/release/auto-update.md`); the server then has to be started
//! again.

use std::process::ExitCode;

fn main() -> ExitCode {
    let build = skate2_net::crypto::Build::from_hex(env!("SKATE2_BUILD_SECRET")).unwrap_or(skate2_net::crypto::Build::DEVELOPMENT);
    let build = verification_build(build);
    build.install();
    let update = skate2_update::Config::for_build(
        option_env!("SKATE2_UPDATE_PUBLIC_KEY"),
        option_env!("SKATE2_UPDATE_REPOSITORY"),
        env!("CARGO_PKG_VERSION"),
        "skate2-server",
    );
    if std::env::args().any(|argument| argument == "--update") {
        return if skate2_update::update_from_command_line(update.as_ref(), "skate2-server") { ExitCode::SUCCESS } else { ExitCode::FAILURE };
    }
    skate2_update::announce_newer_release(update, "skate2-server");
    let bind = std::env::args().skip(1).find(|argument| !argument.starts_with("--"));
    let Some(bind) = skate2_net::server::parse_bind(bind.as_deref()) else {
        eprintln!("usage: skate2-server [ip:port | ip | :port]");
        return ExitCode::FAILURE;
    };
    match skate2_net::server::run(&skate2_net::server::ServerConfig { bind, build }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("dedicated server failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `SKATE2_SERVER_BUILD=development`: the browser build has no build script, so it presents
/// `Build::DEVELOPMENT`, which a development server's random secret refuses
/// (`docs/WEB_PORT_M6_PLAN.md`, phase 5b). A development server run for that verification takes
/// the development build instead. A release build — one carrying its update public key — never
/// does: its secret is the release's, and the variable is reported and ignored.
fn verification_build(build: skate2_net::crypto::Build) -> skate2_net::crypto::Build {
    match std::env::var("SKATE2_SERVER_BUILD").as_deref() {
        Ok("development") if option_env!("SKATE2_UPDATE_PUBLIC_KEY").is_none() => {
            eprintln!(
                "SKATE2_SERVER_BUILD=development: accepting development-build clients (verification only)"
            );
            skate2_net::crypto::Build::DEVELOPMENT
        }
        Ok(value) => {
            eprintln!(
                "SKATE2_SERVER_BUILD={value} ignored: only a development build of the server takes `development`"
            );
            build
        }
        Err(_) => build,
    }
}
