# Registers the /ping slash command with Discord (global).
#
# Usage:
#   $env:DISCORD_APP_ID = "your-application-id"
#   $env:DISCORD_BOT_TOKEN = "your-bot-token"
#   ./scripts/register-commands.ps1
#
# Global commands can take up to an hour to propagate. For instant testing,
# set DISCORD_GUILD_ID and the command is registered to that guild only.

$ErrorActionPreference = "Stop"

$appId = $env:DISCORD_APP_ID
$token = $env:DISCORD_BOT_TOKEN
$guildId = $env:DISCORD_GUILD_ID

if (-not $appId)  { throw "DISCORD_APP_ID is not set" }
if (-not $token)  { throw "DISCORD_BOT_TOKEN is not set" }

if ($guildId) {
    $url = "https://discord.com/api/v10/applications/$appId/guilds/$guildId/commands"
    Write-Host "Registering guild command in guild $guildId ..."
} else {
    $url = "https://discord.com/api/v10/applications/$appId/commands"
    Write-Host "Registering global command (may take up to 1 hour to appear) ..."
}

$body = @{
    name        = "ping"
    description = "Check the bot's latency"
    type        = 1
} | ConvertTo-Json

$resp = Invoke-RestMethod -Method Post -Uri $url -Headers @{
    "Authorization" = "Bot $token"
    "Content-Type"  = "application/json"
} -Body $body

Write-Host "Registered command:" ($resp | ConvertTo-Json -Depth 5)
