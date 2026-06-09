# ping-pong

A tiny Discord **Interactions Endpoint** server written in Rust. It answers the
`/ping` slash command with a detailed latency breakdown, so you can use it to
measure how fast Discord delivers interactions to your host.

```
🏓 Pong!
Discord -> server : 138 ms      # delivery time, derived from the interaction snowflake
Server handling   : 0.072 ms    # time we spent verifying + parsing the request
Interaction id    : 1234567890
Measured at       : 1717971000123 (unix ms)
```

## How it works

Discord delivers every interaction as a signed HTTPS `POST`. The server:

1. Verifies the Ed25519 request signature using your app's **public key**.
2. Answers the `PING` handshake (type 1) with `PONG` — this is what Discord
   sends when you save the endpoint URL.
3. Answers the `/ping` command (type 2) with a latency report.

The "Discord -> server" figure comes from the interaction **snowflake id**: its
top bits encode the millisecond Discord created the interaction, which we
compare against our wall clock. (It includes clock skew between Discord and your
host, so treat it as a relative signal rather than an absolute ground truth.)

## Run locally

```powershell
$env:DISCORD_PUBLIC_KEY = "<your app's public key from the Developer Portal>"
cargo run --release
```

The server listens on `0.0.0.0:8080` by default (`/interactions`). Override the
port with `PORT`.

| Env var               | Required | Description                                    |
| --------------------- | -------- | ---------------------------------------------- |
| `DISCORD_PUBLIC_KEY`  | yes      | App public key (hex) from the Developer Portal |
| `PORT`                | no       | Listen port (default `8080`)                   |

Discord requires a public HTTPS URL. For local testing, expose the port with a
tunnel (e.g. `cloudflared tunnel --url http://localhost:8080` or `ngrok http
8080`) and use that URL below.

## Set it up in Discord

1. Create an application at <https://discord.com/developers/applications>.
2. Copy the **Public Key** (General Information) into `DISCORD_PUBLIC_KEY`.
3. Register the `/ping` command (see below).
4. Under **General Information → Interactions Endpoint URL**, set
   `https://<your-host>/interactions` and save. Discord sends a signed `PING`;
   if the server verifies and replies, the URL is accepted.

### Register the `/ping` command

PowerShell:

```powershell
$env:DISCORD_APP_ID = "<application id>"
$env:DISCORD_BOT_TOKEN = "<bot token>"
# optional: $env:DISCORD_GUILD_ID = "<guild id>"  # instant, instead of global
./scripts/register-commands.ps1
```

Bash:

```bash
export DISCORD_APP_ID="<application id>"
export DISCORD_BOT_TOKEN="<bot token>"
# optional: export DISCORD_GUILD_ID="<guild id>"
./scripts/register-commands.sh
```

Global commands can take up to an hour to appear; set `DISCORD_GUILD_ID` for an
instant, guild-scoped registration while testing.

## Docker

```bash
docker build -t ping-pong .
docker run -e DISCORD_PUBLIC_KEY=<public-key> -p 8080:8080 ping-pong
```

### Docker Compose

`compose.yml` reads your `.env` and exposes port 8080 with a healthcheck:

```bash
docker compose up -d          # pull/run the GHCR image
docker compose up -d --build  # or build locally from source
docker compose logs -f
docker compose down
```

### Auto-built images (GitHub Actions)

`.github/workflows/docker.yml` builds the image on every push to `main` and on
version tags, then pushes it to the GitHub Container Registry (GHCR). No extra
secrets are needed — it uses the built-in `GITHUB_TOKEN`.

Pull a published image with:

```bash
docker pull ghcr.io/<owner>/<repo>:latest
```

Tags produced: `latest` (default branch), the branch name, `sha-<commit>`, and
`vX.Y.Z` / `vX.Y` for git tags. The first push also creates the package; make it
public under the repo's **Packages** settings if you want anonymous pulls.
