# Creator Outreach Workflow

Autonomous TikTok creator outreach pipeline. Discovers creators, drafts personalized messages, follows up, and tracks the full pipeline.

## Components

| Prompt | Purpose | Suggested Schedule |
|--------|---------|-------------------|
| `discover.md` | Find new creators on TikTok | 2x/week |
| `outreach.md` | Draft personalized DMs/emails | Daily |
| `followup.md` | Follow up on non-responses | Daily |
| `pipeline-review.md` | Pipeline health report | Weekly |

## Setup

1. Ensure `nocodebackend-creators` MCP is enabled in config with the creator-app API token
2. Ensure `browser` tool is enabled
3. Add cron jobs:

```
zeroclaw cron add --name "creator-discover" --schedule "0 10 * * 1,4" --prompt-file workflows/lead-gen/creator-outreach/prompts/discover.md
zeroclaw cron add --name "creator-outreach" --schedule "0 11 * * *" --prompt-file workflows/lead-gen/creator-outreach/prompts/outreach.md
zeroclaw cron add --name "creator-followup" --schedule "0 14 * * *" --prompt-file workflows/lead-gen/creator-outreach/prompts/followup.md
zeroclaw cron add --name "creator-review" --schedule "0 9 * * 1" --prompt-file workflows/lead-gen/creator-outreach/prompts/pipeline-review.md
```

## Database

Uses the `creators` table in NCB instance `36905_creator_app` via the `nocodebackend-creators` MCP server.

## Tools Used

- `browser` — TikTok browsing, creator research
- `nocodebackend-creators` MCP — CRUD on creators table
- `memory` — pipeline reports and session notes
