You are running unattended overnight. No human will answer questions.
Do exactly ONE cycle, then exit.

## Start
Read CLAUDE.md, BENCH.md, SUBMISSIONS.md, and MORNING.md.

## Cycle
1. Pick the single most promising untried optimization from BENCH.md
   (or the profile). Implement it.
2. Run the full verification loop: fixtures, make check-dev,
   diff_vs_ref on seeds 2-5, bench.sh at 20M and 60M.
3. If anything fails or it's slower: revert, log why in BENCH.md, and
   try ONE more idea if there's time. No submission this cycle.
4. Submit ONLY if all checks pass AND the 20M bench is at least 5%
   faster than the last submitted version AND 60M is under 60s.

## Submitting
- Commit, push, and record the full commit SHA.
- In Chrome, open the hackathon submission page
  (https://www.joinbuilders.org/buildathon/tracks/shopping-sessions)
  and submit the repo URL and that SHA. Submit ONCE.
- Wait for the result. Record the time, SHA, result, score, and any
  missing/extra/differing row counts in SUBMISSIONS.md.
- If it failed on the dev set: read the differing rows, log the cause,
  and fix it in the next cycle.

## Never
- Force-push, rebase, or rewrite git history (every submitted commit
  must stay fetchable)
- Change repo visibility, commit any data, or go past 50 MB
- Change which commit is chosen for the final
- Submit more than once per cycle
- Retry a failing browser action more than 3 times. If Chrome breaks,
  log it in MORNING.md and keep optimizing without submitting.

## End
Update BENCH.md. Add 2-3 lines to MORNING.md: what you tried, what
was kept, best score so far, any blockers. Then exit.
