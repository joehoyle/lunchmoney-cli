# lunchmoney

An agent-friendly CLI for [Lunch Money API v2](https://lunchmoney.dev). Interactive terminals get compact, resource-aware tables; pipes get stable JSON. Transaction tables resolve category and account IDs into names and automatically fit the terminal width.

## Install and authenticate

```sh
cargo install --path .
lunchmoney auth login
```

`auth login` securely prompts for your token and saves it for this user, so every shell can use `lunchmoney` without additional setup. On macOS it is saved in `~/Library/Application Support/lunchmoney/config.json`; on Linux it uses `$XDG_CONFIG_HOME/lunchmoney/config.json` (or `~/.config/lunchmoney/config.json`). The directory and token file are restricted to your user on Unix.

You can still set `LUNCH_MONEY_TOKEN` or pass `--token` for a single invocation. Those take precedence over the saved token. Run `lunchmoney auth status` to check configuration without exposing the token, or `lunchmoney auth logout` to remove it.

## Examples

```sh
lunchmoney me
lunchmoney transactions list
lunchmoney transactions list --month 2026-01
lunchmoney transactions list --wide
lunchmoney transactions list --exclude-pending
lunchmoney --output json categories list | jq '.categories[].name'
lunchmoney categories create --body '{"name":"Coffee","is_income":false}'
lunchmoney transactions create --body @transaction.json
lunchmoney budgets delete --category-id 42 --start-date 2026-01-01
lunchmoney raw get /recurring_items
lunchmoney completion zsh > ~/.zfunc/_lunchmoney
```

Use `lunchmoney help`, `lunchmoney <command> --help`, and `lunchmoney help <command>` for detailed command documentation.

## Output modes

- `--output auto` (default): tables at a terminal, JSON when piped
- `--output table`: always use human-readable output
- `--output json`: always preserve the API response shape
- `--compact`: emit single-line JSON
- `--wide`: add secondary columns such as transaction status, tags, notes, and source

Tables use friendly labels, right-align monetary values, format currency consistently, truncate long cells with an ellipsis, and display a result count. Set `COLUMNS` to override terminal-width detection.

Pending transactions are included by default. Use `--exclude-pending` to omit them. Transaction tables use color when shown in a terminal. IDs are hidden by default; use `--wide` to include the ID, status, tags, notes, and source columns.
