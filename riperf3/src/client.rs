// riperf3/riperf3/src/client.rs
// Implements the Client types and functions for the riperf3 project.

use crate::error::ConfigError;
use crate::utils::DEFAULT_PORT;

// Client-specific struct
pub struct Client {
    host: String,
    port: u16,
    //TODO: Add fields
}

// Implement client-specific functions
impl Client {
    pub async fn run(&self) -> Result<(), ConfigError> {
        vprintln!("Client connecting to: {}:{}", self.host, self.port);
        //TODO: Implement client logic

        Ok(())
    }

    //TODO: Add additional functions
}

// Client builder struct
pub struct ClientBuilder {
    host: Option<String>,
    port: Option<u16>,
    //TODO: Add fields
}

impl ClientBuilder {
    pub fn new() -> Self {
        Self {
            host: None,
            port: None,
            //TODO: Initialize fields
        }
    }

    pub fn host(mut self, host: &str) -> Self {
        self.host = Some(host.to_string());
        self
    }

    pub fn port(mut self, port: Option<u16>) -> Self {
        self.port = port;
        self
    }

    //TODO: Add methods for additional fields

    // Build function to produce a Client struct
    pub fn build(self) -> Result<Client, ConfigError> {
        // Validate required fields
        Ok(Client {
            // Initialize Client with validated fields
            host: self.host.ok_or(ConfigError::MissingField("host"))?,

            // If there is no port, use DEFAULT_PORT
            port: self.port.unwrap_or(DEFAULT_PORT),

            // TODO: Initialize additional fields
        })
    }
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}
