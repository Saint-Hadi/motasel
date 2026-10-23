# Motasel

**Motasel** is a lightweight, self-hosted file-sharing web application written in Rust. It runs an HTTP server on your local network, allowing you to access the web interface from a browser on your computer or another device connected to the same network.

## Features

- **Lightweight backend:** Built with Rust.
- **Local network access:** Listens on `0.0.0.0:8080` by default.
- **Browser-based interface:** Access the application without installing a client on other devices.
- **Local file storage:** Stores application files in the project's `storage/` directory by default.
- **Configurable upload limit:** The default maximum upload size is 1 GiB.

## Requirements

- Rust and Cargo
- A web browser
- Linux or another operating system supported by the project

## Getting Started

### 1. Clone the repository

```bash
git clone https://github.com/YOUR_USERNAME/motasel.git
cd motasel
```

Replace `YOUR_USERNAME` with your GitHub username.

### 2. Run the application

```bash
cargo run
```

The development server listens on port `8080` by default.

### 3. Open the web interface

On the same computer, visit:

```text
http://localhost:8080
```

To access Motasel from another device on the same Wi-Fi or local network, use the computer's LAN IP address. For example:

```text
http://192.168.1.7:8080
```

Replace the example IP address with the address printed by the application on your computer.

## Storage

By default, files are stored in:

```text
./storage/
```

Make sure the application has permission to read and write to this directory.

## Configuration

The current startup configuration reports the following defaults:

| Setting | Default |
|---|---|
| Listen address | `0.0.0.0` |
| Port | `8080` |
| Maximum upload size | `1 GiB` |
| Storage directory | `./storage/` |

Consult the source code for the supported configuration options.

## Development

Build the application:

```bash
cargo build
```

Run it in development mode:

```bash
cargo run
```

Check the code with Clippy:

```bash
cargo clippy --all-targets --all-features
```

Run the test suite:

```bash
cargo test
```

## Security Notes

- Use Motasel on trusted networks unless you have configured appropriate access controls.
- Do not expose the server directly to the public internet without reviewing authentication, authorization, and network security.
- Do not commit private files, credentials, environment variables, or personal uploads to Git.

## License

No license has been specified yet. Add a `LICENSE` file before distributing the project under an open-source license.

---

*Built with Rust.*
