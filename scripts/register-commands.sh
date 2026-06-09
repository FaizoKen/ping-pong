#!/usr/bin/env bash
# Registers the /ping slash command with Discord (global).
#
# Usage:
#   export DISCORD_APP_ID="your-application-id"
#   export DISCORD_BOT_TOKEN="your-bot-token"
#   ./scripts/register-commands.sh
#
# Global commands can take up to an hour to propagate. For instant testing,
# set DISCORD_GUILD_ID and the command is registered to that guild only.
set -euo pipefail

: "${DISCORD_APP_ID:?DISCORD_APP_ID is not set}"
: "${DISCORD_BOT_TOKEN:?DISCORD_BOT_TOKEN is not set}"

if [[ -n "${DISCORD_GUILD_ID:-}" ]]; then
  url="https://discord.com/api/v10/applications/${DISCORD_APP_ID}/guilds/${DISCORD_GUILD_ID}/commands"
  echo "Registering guild command in guild ${DISCORD_GUILD_ID} ..."
else
  url="https://discord.com/api/v10/applications/${DISCORD_APP_ID}/commands"
  echo "Registering global command (may take up to 1 hour to appear) ..."
fi

curl -sS -X POST "$url" \
  -H "Authorization: Bot ${DISCORD_BOT_TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"name":"ping","description":"Check the bot'\''s latency","type":1}'
echo
