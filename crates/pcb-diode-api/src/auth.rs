use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use rand::Rng;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub email: Option<String>,
}

impl AuthTokens {
    pub fn is_expired(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
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
    fs::create_dir_all(&pcb_dir)?;
    Ok(pcb_dir.join("auth.toml"))
}

pub fn load_tokens() -> Result<Option<AuthTokens>> {
    let path = get_auth_file_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(&path)?;
    Ok(Some(toml::from_str(&contents)?))
}

fn save_tokens(
    access_token: &str,
    refresh_token: &str,
    expires_at: i64,
    email: Option<&str>,
) -> Result<()> {
    let tokens = AuthTokens {
        access_token: access_token.to_string(),
        refresh_token: refresh_token.to_string(),
        expires_at,
        email: email.map(|s| s.to_string()),
    };
    let contents = toml::to_string(&tokens)?;
    fs::write(get_auth_file_path()?, contents)?;
    Ok(())
}

fn clear_tokens() -> Result<()> {
    let path = get_auth_file_path()?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

#[derive(Serialize)]
struct RefreshRequest {
    refresh_token: String,
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    refresh_token: String,
    expires_at: i64,
}

pub fn refresh_tokens() -> Result<AuthTokens> {
    let tokens = load_tokens()?.context("No tokens to refresh")?;
    let api_url = crate::get_api_base_url();
    let url = format!("{}/api/auth/refresh", api_url);

    let response = Client::new()
        .post(&url)
        .json(&RefreshRequest {
            refresh_token: tokens.refresh_token.clone(),
        })
        .send()?;

    if !response.status().is_success() {
        anyhow::bail!("Token refresh failed: {}", response.status());
    }

    let refresh_response: RefreshResponse = response.json()?;

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

pub fn get_valid_token() -> Result<String> {
    let tokens =
        load_tokens()?.context("Not authenticated. Run `pcb auth login` to authenticate.")?;

    if tokens.is_expired() {
        let new_tokens = refresh_tokens()?;
        return Ok(new_tokens.access_token);
    }

    Ok(tokens.access_token)
}

pub fn login() -> Result<()> {
    let code: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(6)
        .map(char::from)
        .collect::<String>()
        .to_uppercase();

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://localhost:{}/callback", port);

    let base_url = crate::get_web_base_url();
    let auth_url = format!(
        "{}/cli-auth?code={}&redirect_uri={}",
        base_url,
        code,
        urlencoding::encode(&redirect_uri)
    );

    println!("Code: {}", code);
    println!("Opening browser...");

    if let Err(e) = open::that(&auth_url) {
        eprintln!("Failed to open browser: {}", e);
        eprintln!("Please manually open: {}", auth_url);
    }

    let (mut stream, _) = listener.accept()?;

    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    let tokens = parse_tokens_from_request(&request_line)?;

    let response = format!(
        "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\n\r\n",
        base_url
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;

    save_tokens(
        &tokens.access_token,
        &tokens.refresh_token,
        tokens.expires_at,
        tokens.email.as_deref(),
    )?;

    println!("✓ Authentication successful!");
    if let Some(email) = &tokens.email {
        println!("  Logged in as: {}", email);
    }

    Ok(())
}

pub fn logout() -> Result<()> {
    clear_tokens()?;
    println!("✓ Logged out successfully");
    Ok(())
}

pub fn status() -> Result<()> {
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
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        anyhow::bail!("Invalid HTTP request format");
    }

    let query_string = parts[1].split('?').nth(1).context("No query string")?;

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

    Ok(CallbackTokens {
        access_token: access_token.context("Missing access_token")?,
        refresh_token: refresh_token.context("Missing refresh_token")?,
        expires_at: expires_at.context("Missing expires_at")?.parse()?,
        email: None,
    })
}

#[derive(Args, Debug)]
#[command(about = "Manage authentication")]
pub struct AuthArgs {
    #[command(subcommand)]
    command: Option<AuthCommand>,
}

#[derive(Subcommand, Debug)]
pub enum AuthCommand {
    Login,
    Logout,
    Status,
}

pub fn execute(args: AuthArgs) -> Result<()> {
    match args.command {
        Some(AuthCommand::Login) | None => login(),
        Some(AuthCommand::Logout) => logout(),
        Some(AuthCommand::Status) => status(),
    }
}
