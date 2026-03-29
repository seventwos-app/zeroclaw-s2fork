# Pipeline Review Task

Review the full creator pipeline and surface actionable insights.

## Steps

1. Query all creators from the database
2. Generate a summary report:
   - Total creators by status (discovered, contacted, responded, in talks, onboarded, declined)
   - Conversion rates between stages
   - Creators needing follow-up (follow_up_date is today or past)
   - High-priority creators requiring attention
   - Creators who responded but haven't been moved to "in talks"
3. Flag any stale entries (status unchanged for 14+ days)
4. Recommend next actions:
   - Which creators to prioritize for outreach
   - Which follow-ups are overdue
   - Whether discovery needs to run (pipeline running low)
5. Save the report to memory with tag "pipeline-review"
