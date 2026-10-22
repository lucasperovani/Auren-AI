# Instagram Skill

Account information and publishing through the official Instagram Graph API.

## When to use

- The user asks about their account or recent posts.
- The user asks to publish content.

## Tools

| Tool | Risk | Behavior |
|---|---|---|
| `instagram_get_account_info` | low | Account basics |
| `instagram_list_media` | low | Recent media |
| `instagram_publish_media` | high | Publish an image; **requires explicit approval** |

## Rules

- Publishing always goes through the approval engine by default; the approval shows the image URL and caption.
- Only operations supported by the official API and the account type are implemented. No credential scraping and no bypassing of platform restrictions.
- Only business/creator accounts support publishing through the Graph API; personal accounts are read-only.

## Requirements

A connected Instagram business/creator account with a Meta access token. Connect it in the Integrations view.