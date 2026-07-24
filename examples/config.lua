-- rs-mrxvt example Lua config.
--
-- Copy to ~/.config/rs-mrxvt/config.lua and edit.
-- Requires building rs-mrxvt with --features lua.
--
-- The script must `return` a table with the same shape as the TOML config.
-- Because it's Lua, you can use logic: conditionals, env vars, time, etc.

-- ──────────────────────────────────────────────────────────────────────────
-- Dynamic: pick theme based on time of day
-- ──────────────────────────────────────────────────────────────────────────
local hour = tonumber(os.date("%H"))
local theme = "mrxvt"
if hour >= 20 or hour < 6 then
  theme = "tokyo-night"
end

-- ──────────────────────────────────────────────────────────────────────────
-- Read the user's preferred shell from the environment, but prefer bash
-- ──────────────────────────────────────────────────────────────────────────
local function pick_shell()
  if os.getenv("MRXVT_SHELL") then
    return os.getenv("MRXVT_SHELL")
  end
  -- Try bash first (most common default), then zsh, then $SHELL, then /bin/sh.
  for _, candidate in ipairs({"/bin/bash", "/usr/bin/bash", "/bin/zsh", "/usr/bin/zsh"}) do
    local f = io.open(candidate, "r")
    if f then f:close() return candidate end
  end
  return os.getenv("SHELL") or "/bin/sh"
end

local shell = pick_shell()

-- ──────────────────────────────────────────────────────────────────────────
-- Detect if we're on a remote host (SSH) and adjust tabs accordingly
-- ──────────────────────────────────────────────────────────────────────────
local is_ssh = os.getenv("SSH_CLIENT") ~= nil

-- ──────────────────────────────────────────────────────────────────────────
-- Return the config table
-- ──────────────────────────────────────────────────────────────────────────
return {
  -- Terminal defaults
  terminal = {
    cols = 120,
    rows = 40,
    scrollback = 10000,
    shell = shell,
  },

  -- UI behavior
  ui = {
    always_show_tabs = true,
    disable_palette = false,
    tabbar_height = 1,
    theme = theme,           -- dynamic!
    focus = "click",
  },

  -- Default profile name
  default_profile = "default",

  -- Profile definitions
  profiles = {
    default = {
      command = { shell },
    },

    -- zsh profile — launched with Alt+Z
    zsh = {
      command = { "zsh" },
    },

    -- Example: web server fleet, all tagged "web" for group broadcasting
    web1 = {
      command = { "ssh", "user@web-01.example.com" },
      tag = "web",
    },
    web2 = {
      command = { "ssh", "user@web-02.example.com" },
      tag = "web",
    },
    web3 = {
      command = { "ssh", "user@web-03.example.com" },
      tag = "web",
    },

    -- Example: a database admin tab (untagged — won't receive group broadcasts)
    db = {
      command = { "ssh", "user@db-01.example.com" },
      -- no tag → only receives input when active or when broadcast = All
    },

    -- Example: monitoring tab with custom env
    monitoring = {
      command = { "htop" },
      env = {
        TERM = "xterm-256color",
        HTOPRC = "/home/user/.config/htop/mrxvt-rc",
      },
    },
  },

  -- Macros: chord → command name
  macros = {
    ["Ctrl+Shift+R"] = "ResetTerminal",
    -- ["Ctrl+Shift+B"] = "ToggleBroadcastAll",
  },
}
