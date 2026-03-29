# Creator Discovery Task

Find new TikTok creators matching the target profile for the creator program.

## Steps

1. Use the browser to browse TikTok trending pages, relevant hashtags, and creator directories
2. For each promising creator, extract:
   - Name / display name
   - TikTok handle (@username)
   - Niche / content category
   - Approximate follower count
   - Contact info (if visible: email in bio, link in bio, etc.)
   - Brief notes on content quality and relevance
3. Save each creator to the database via nocodebackend-creators MCP:
   - status: "discovered"
   - priority: "medium" (upgrade to "high" if >100k followers or perfect niche fit)
   - platform: "TikTok"
   - tags: relevant niche tags
   - log: initial discovery entry with date
4. Skip any creator already in the database
5. Aim to discover 10-15 new creators per run

## Target Profile

- Platform: TikTok (primary), Instagram/YouTube (secondary)
- Niche: Align with the app's target audience
- Minimum followers: 5,000
- Engagement signals: consistent posting, good comment engagement, authentic audience
- Bonus: already promotes apps/products, has link in bio, email in bio
