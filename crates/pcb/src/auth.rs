use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use colored::Colorize;
use rand::Rng;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;

fn get_api_base_url() -> String {
    if let Ok(url) = std::env::var("DIODE_API_URL") {
        return url;
    }

    #[cfg(debug_assertions)]
    return "http://localhost:3001".to_string();
    #[cfg(not(debug_assertions))]
    return "https://api.diode.computer".to_string();
}

fn get_web_base_url() -> String {
    if let Ok(url) = std::env::var("DIODE_APP_URL") {
        return url;
    }

    #[cfg(debug_assertions)]
    return "http://localhost:3000".to_string();
    #[cfg(not(debug_assertions))]
    return "https://app.diode.computer".to_string();
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64, // Unix timestamp in seconds
    pub email: Option<String>,
}

impl AuthTokens {
    pub fn is_expired(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // Consider expired if less than 5 minutes remaining
        self.expires_at - now < 300
    }

    pub fn time_until_expiry(&self) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let remaining = self.expires_at - now;

        if remaining <= 0 {
            "expired".to_string()
        } else if remaining < 3600 {
            format!("{} minutes", remaining / 60)
        } else if remaining < 86400 {
            format!("{} hours", remaining / 3600)
        } else {
            format!("{} days", remaining / 86400)
        }
    }
}

fn get_auth_file_path() -> Result<PathBuf> {
    let home_dir = dirs::home_dir().context("Failed to get home directory")?;
    let pcb_dir = home_dir.join(".pcb");
    fs::create_dir_all(&pcb_dir).context("Failed to create .pcb directory")?;
    Ok(pcb_dir.join("auth.toml"))
}

pub fn load_tokens() -> Result<Option<AuthTokens>> {
    let auth_file_path = get_auth_file_path()?;

    if !auth_file_path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&auth_file_path).context("Failed to read auth.toml")?;

    let tokens: AuthTokens = toml::from_str(&contents).context("Failed to parse auth.toml")?;

    Ok(Some(tokens))
}

pub fn save_tokens(
    access_token: &str,
    refresh_token: &str,
    expires_at: i64,
    email: Option<&str>,
) -> Result<()> {
    let auth_file_path = get_auth_file_path()?;

    let tokens = AuthTokens {
        access_token: access_token.to_string(),
        refresh_token: refresh_token.to_string(),
        expires_at,
        email: email.map(|s| s.to_string()),
    };

    let contents = toml::to_string(&tokens).context("Failed to serialize auth tokens")?;

    fs::write(&auth_file_path, contents).context("Failed to write auth.toml")?;

    Ok(())
}

#[derive(Serialize)]
struct RefreshTokenRequest {
    refresh_token: String,
}

#[derive(Deserialize)]
struct RefreshTokenResponse {
    access_token: String,
    refresh_token: String,
    expires_at: i64, // Unix timestamp
}

pub fn refresh_tokens() -> Result<AuthTokens> {
    let tokens = load_tokens()?.context("No tokens to refresh")?;

    let client = Client::new();
    let api_base_url = get_api_base_url();
    let url = format!("{}/api/auth/refresh", api_base_url);

    let request_body = RefreshTokenRequest {
        refresh_token: tokens.refresh_token.clone(),
    };

    let response = client
        .post(&url)
        .json(&request_body)
        .send()
        .context("Failed to refresh token")?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().unwrap_or_default();
        anyhow::bail!("Token refresh failed ({}): {}", status, error_text);
    }

    let refresh_response: RefreshTokenResponse = response
        .json()
        .context("Failed to parse refresh response")?;

    // Save new tokens
    save_tokens(
        &refresh_response.access_token,
        &refresh_response.refresh_token,
        refresh_response.expires_at,
        tokens.email.as_deref(),
    )?;

    Ok(AuthTokens {
        access_token: refresh_response.access_token,
        refresh_token: refresh_response.refresh_token,
        expires_at: refresh_response.expires_at,
        email: tokens.email,
    })
}

fn clear_tokens() -> Result<()> {
    let auth_file_path = get_auth_file_path()?;

    if auth_file_path.exists() {
        fs::remove_file(&auth_file_path).context("Failed to remove auth.toml")?;
    }

    Ok(())
}

#[derive(Args, Debug)]
#[command(about = "Manage authentication")]
pub struct AuthArgs {
    #[command(subcommand)]
    command: Option<AuthCommand>,
}

#[derive(Subcommand, Debug)]
enum AuthCommand {
    /// Log in to Diode (opens browser)
    Login(LoginArgs),
    /// Log out and clear stored tokens
    Logout,
    /// Show current authentication status
    Status,
}

#[derive(Args, Debug)]
struct LoginArgs {}

pub fn execute(args: AuthArgs) -> Result<()> {
    match args.command {
        Some(AuthCommand::Login(_)) | None => login(),
        Some(AuthCommand::Logout) => logout(),
        Some(AuthCommand::Status) => status(),
    }
}

fn login() -> Result<()> {
    // Generate 6-character alphanumeric code
    let code: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(6)
        .map(char::from)
        .collect::<String>()
        .to_uppercase();

    // Start TCP listener on random port
    let listener = TcpListener::bind("127.0.0.1:0").context("Failed to bind to local address")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://localhost:{}/callback", port);

    // Get web base URL
    let base_url = get_web_base_url();

    // Construct auth URL
    let auth_url = format!(
        "{}/cli-auth?code={}&redirect_uri={}",
        base_url,
        code,
        urlencoding::encode(&redirect_uri)
    );

    // Display code to user
    println!("{} {}", "Code:".dimmed(), code.bold().cyan());
    println!("{}", "Opening browser...".dimmed());

    // Open browser
    if let Err(e) = open::that(&auth_url) {
        eprintln!("{}", format!("Failed to open browser: {}", e).yellow());
        eprintln!("Please manually open: {}", auth_url);
    }

    let (mut stream, _) = listener.accept().context("Failed to accept connection")?;

    // Read HTTP request
    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .context("Failed to read request")?;

    // Parse query string from request line
    // Format: GET /callback?access_token=...&refresh_token=... HTTP/1.1
    let tokens = parse_tokens_from_request(&request_line)?;

    // Send 302 redirect response to close the browser tab
    let response = format!(
        "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\n\r\n",
        base_url
    );
    stream
        .write_all(response.as_bytes())
        .context("Failed to send response")?;
    stream.flush()?;

    // Save tokens
    save_tokens(
        &tokens.access_token,
        &tokens.refresh_token,
        tokens.expires_at,
        tokens.email.as_deref(),
    )?;

    println!("{}", "✓ Authentication successful!".green().bold());
    if let Some(email) = &tokens.email {
        println!("  Logged in as: {}", email.cyan());
    }

    Ok(())
}

fn logout() -> Result<()> {
    clear_tokens()?;
    println!("{}", "✓ Logged out successfully".green());
    Ok(())
}

fn status() -> Result<()> {
    match load_tokens()? {
        Some(tokens) => {
            println!("Authentication Status:");
            println!("  Status: Logged in");
            if let Some(email) = &tokens.email {
                println!("  Email: {}", email);
            }
            if tokens.is_expired() {
                println!("  Token: expired");
                println!("\nRun `pcb auth login` to re-authenticate.");
            } else {
                println!("  Token expires in: {}", tokens.time_until_expiry());
            }
        }
        None => {
            println!("Authentication Status:");
            println!("  Status: Not logged in");
            println!("\nRun `pcb auth login` to authenticate.");
        }
    }
    Ok(())
}

struct CallbackTokens {
    access_token: String,
    refresh_token: String,
    expires_at: i64,
    email: Option<String>,
}

fn parse_tokens_from_request(request_line: &str) -> Result<CallbackTokens> {
    // Extract query string from request line
    // Format: GET /callback?access_token=...&refresh_token=...&expires_at=... HTTP/1.1
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        anyhow::bail!("Invalid HTTP request format");
    }

    let path_and_query = parts[1];
    let query_string = path_and_query
        .split('?')
        .nth(1)
        .context("No query string in callback")?;

    // Parse query parameters
    let mut access_token = None;
    let mut refresh_token = None;
    let mut expires_at = None;

    for param in query_string.split('&') {
        let mut kv = param.split('=');
        let key = kv.next().context("Invalid query parameter")?;
        let value = kv.next().context("Invalid query parameter")?;
        let decoded_value = urlencoding::decode(value)?.into_owned();

        match key {
            "access_token" => access_token = Some(decoded_value),
            "refresh_token" => refresh_token = Some(decoded_value),
            "expires_at" => expires_at = Some(decoded_value),
            _ => {}
        }
    }

    let access_token = access_token.context("Missing access_token in callback")?;
    let refresh_token = refresh_token.context("Missing refresh_token in callback")?;
    let expires_at_str = expires_at.context("Missing expires_at in callback")?;
    let expires_at: i64 = expires_at_str.parse().context("Invalid expires_at value")?;

    Ok(CallbackTokens {
        access_token,
        refresh_token,
        expires_at,
        email: None, // Email will be fetched from API if needed
    })
}
