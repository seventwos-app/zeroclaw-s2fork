# Follow-Up Task

Follow up with creators who haven't responded to outreach.

## Steps

1. Query creators database for status "contacted" where follow_up_date is today or past
2. For each creator:
   a. Check their log for how many follow-ups have been sent
   b. If 3+ follow-ups with no response → set status to "declined", add log entry
   c. Otherwise, draft a follow-up message that:
      - Doesn't repeat the original pitch
      - References something new (a recent video, a program update, social proof)
      - Keeps it short (under 80 words)
      - Has a different CTA than the original
   d. Add log entry: "Follow-up #N drafted — [date]"
   e. Set follow_up_date to 5 days from now

## Follow-Up Angles (rotate through these)

1. **New content hook**: "Saw your latest [video topic] — even more convinced you'd be great for this!"
2. **Social proof**: "Just onboarded [X] other creators in [niche] — thought of you again"
3. **Value add**: "Quick update — we just added [new perk] to the program"
4. **Direct ask**: "Totally understand if the timing isn't right — just wanted to check one last time"
