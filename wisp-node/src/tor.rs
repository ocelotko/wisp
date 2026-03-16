use anyhow::{anyhow, Context, Result};
use log::info;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

pub struct TorService {
    pub onion_address: String,
    pub socks_address: String,
    pub private_key: String,
    _stream: BufReader<TcpStream>,
}

/// Connects to a Tor control port and creates a new v3 onion service.
/// Returns the onion service address (e.g., "xyz.onion:9000") and the SOCKS proxy address.
pub async fn create_hidden_service(
    control_addr: &str,
    local_port: u16,
    private_key: Option<String>,
) -> Result<TorService> {
    info!("Connecting to Tor control port at {}...", control_addr);
    let stream = TcpStream::connect(control_addr)
        .await
        .context("Failed to connect to Tor control port. Is Tor running?")?;

    let mut stream = BufReader::new(stream);

    // 1. Authenticate
    authenticate(&mut stream).await?;
    info!("Authenticated with Tor controller successfully.");

    // 2. Get SOCKS info
    let socks_addr = get_socks_addr(&mut stream).await?;

    // 3. Create Onion Service
    let (onion_addr, pk) = add_onion(&mut stream, local_port, private_key).await?;

    Ok(TorService {
        onion_address: onion_addr,
        socks_address: socks_addr,
        private_key: pk,
        _stream: stream,
    })
}

async fn send_command(stream: &mut BufReader<TcpStream>, cmd: &str) -> Result<Vec<String>> {
    stream.write_all(cmd.as_bytes()).await?;
    stream.write_all(b"\r\n").await?;
    stream.flush().await?;

    let mut response_lines = Vec::new();
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = stream.read_line(&mut line).await?;
        if bytes_read == 0 {
            return Err(anyhow!("Tor control connection closed unexpectedly"));
        }

        let trimmed = line.trim_end();
        if trimmed.len() < 3 {
            return Err(anyhow!("Invalid response from Tor: {}", trimmed));
        }

        let code = &trimmed[0..3];
        let separator = trimmed.chars().nth(3).unwrap_or(' ');
        let content = if trimmed.len() > 4 { &trimmed[4..] } else { "" };

        if code != "250" {
            // 250 is OK. Errors are usually 4xx or 5xx.
            return Err(anyhow!("Tor control error: {}", trimmed));
        }

        response_lines.push(content.to_string());

        if separator == ' ' {
            break;
        }
    }
    Ok(response_lines)
}

async fn authenticate(stream: &mut BufReader<TcpStream>) -> Result<()> {
    // Try PROTOCOLINFO to find auth methods
    let lines = send_command(stream, "PROTOCOLINFO 1").await?;

    let mut cookie_file = None;

    for line in lines {
        if line.starts_with("AUTH") {
            // Example: AUTH METHODS=COOKIE,SAFECOOKIE COOKIEFILE="/var/run/tor/control.authcookie"
            if let Some(idx) = line.find("COOKIEFILE=") {
                let remainder = &line[idx + "COOKIEFILE=".len()..];
                if remainder.starts_with('"') {
                    if let Some(end_quote) = remainder[1..].find('"') {
                        cookie_file = Some(remainder[1..end_quote + 1].to_string());
                    }
                } else {
                    let end = remainder.find(' ').unwrap_or(remainder.len());
                    cookie_file = Some(remainder[..end].to_string());
                }
            }
        }
    }

    if let Some(path) = cookie_file {
        info!(
            "Tor requires cookie authentication. Reading cookie from {}",
            path
        );
        let cookie_bytes = tokio::fs::read(&path).await.context(format!(
            "Failed to read Tor auth cookie at {}. Check permissions.",
            path
        ))?;
        let hex_cookie = hex::encode(cookie_bytes);
        send_command(stream, &format!("AUTHENTICATE {}", hex_cookie)).await?;
    } else {
        // Try empty auth (no password/cookie)
        send_command(stream, "AUTHENTICATE").await?;
    }

    Ok(())
}

async fn get_socks_addr(stream: &mut BufReader<TcpStream>) -> Result<String> {
    let lines = send_command(stream, "GETINFO net/listeners/socks").await?;
    // Response: net/listeners/socks=127.0.0.1:9050

    for line in lines {
        if let Some(val) = line.strip_prefix("net/listeners/socks=") {
            // val might be "127.0.0.1:9050" or "127.0.0.1:9050 [::1]:9050"
            // We take the first one.
            let first = val.split_whitespace().next().unwrap_or("");
            return Ok(first.trim_matches('"').to_string());
        }
    }
    Err(anyhow!("Could not find SOCKS address in Tor response"))
}

async fn add_onion(
    stream: &mut BufReader<TcpStream>,
    local_port: u16,
    private_key: Option<String>,
) -> Result<(String, String)> {
    // If we have a key, use it. Otherwise ask for a new best key (ED25519-V3).
    let key_arg = private_key.as_deref().unwrap_or("NEW:BEST");
    let cmd = format!("ADD_ONION {} Port={1},127.0.0.1:{1}", key_arg, local_port);
    let lines = send_command(stream, &cmd).await?;

    let mut service_id = None;
    let mut ret_private_key = None;

    for line in lines {
        if let Some(val) = line.strip_prefix("ServiceID=") {
            service_id = Some(val.to_string());
        } else if let Some(val) = line.strip_prefix("PrivateKey=") {
            ret_private_key = Some(val.to_string());
        }
    }

    let service_id = service_id
        .ok_or_else(|| anyhow!("Failed to create onion service: No ServiceID returned"))?;
    // If we passed a key, Tor might not return it back. If we generated one, it must return it.
    let final_pk = ret_private_key
        .or(private_key)
        .ok_or_else(|| anyhow!("No private key available"))?;

    Ok((format!("{}.onion:{}", service_id, local_port), final_pk))
}
