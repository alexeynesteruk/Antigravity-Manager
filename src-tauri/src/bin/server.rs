use std::env;

#[derive(Debug, Default, PartialEq)]
struct ServerOptions {
    port: Option<u16>,
    host: Option<String>,
    disable_web: bool,
}

fn parse_server_options(args: impl IntoIterator<Item = String>) -> Result<ServerOptions, String> {
    let mut options = ServerOptions::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" | "-p" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("Missing value for {arg}"))?;
                options.port = Some(
                    value
                        .parse::<u16>()
                        .ok()
                        .filter(|port| *port > 0)
                        .ok_or_else(|| format!("Invalid port number: {value}"))?,
                );
            }
            "--host" | "-h" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("Missing value for {arg}"))?;
                let mut config = antigravity_tools_lib::proxy::ProxyConfig::default();
                config.apply_headless_bind_overrides(Some(&value), None, false)?;
                options.host = config.bind_address;
            }
            "--no-web" | "--pure-proxy" => options.disable_web = true,
            "--headless" => {}
            _ => return Err(format!("Unknown server option: {arg}")),
        }
    }
    Ok(options)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    // Check for help flags specifically for server first
    if args.iter().any(|a| a == "--help") {
        println!(
            "Antigravity Headless Server\n\n\
            Usage:\n  antigravity-server [OPTIONS] [COMMAND]\n\n\
            Options:\n  \
            -p, --port <PORT>        Set listening port (default: 8045)\n  \
            -h, --host <IP>          Bind this exact IPv4/IPv6 address or localhost\n  \
            --no-web, --pure-proxy   Disable Web UI static console (pure proxy mode)\n  \
            --help                   Show help information\n\n\
            Commands:\n  \
            switch, list, current, import, export, sync  (Run 'antigravity-tools help' for CLI command details)\n"
        );
        return Ok(());
    }

    // Check for CLI subcommands (switch, list, current, status, etc.)
    match antigravity_tools_lib::modules::cli_handler::try_handle_cli(&args).await {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(e) => {
            eprintln!("\x1b[31m[Antigravity CLI Error]\x1b[0m {}", e);
            std::process::exit(1);
        }
    }

    let options = parse_server_options(args.into_iter().skip(1))
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    antigravity_tools_lib::run_headless_server(options.port, options.host, options.disable_web)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<ServerOptions, String> {
        parse_server_options(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn validates_server_flags_without_silently_using_defaults() {
        for args in [
            vec!["--port"],
            vec!["--host"],
            vec!["--port", "0"],
            vec!["-p", "65536"],
            vec!["--host", "bad-host"],
            vec!["--unknown"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
        assert_eq!(
            parse(&["-p", "8046", "-h", "::1", "--no-web"]).unwrap(),
            ServerOptions {
                port: Some(8046),
                host: Some("::1".into()),
                disable_web: true
            }
        );
        assert_eq!(parse(&[]).unwrap(), ServerOptions::default());
    }
}
