# SUBMISSIONS.md: every submission on the track page

- Track page: https://www.joinbuilders.org/buildathon/tracks/shopping-sessions
- Repository to submit: https://github.com/Vibhor7-7/shopping-sessions (team VibDiv)
- **The last submitted version is the last row of the table below.**

## How the page works (as seen on 2026-11-21)

- The form has two fields, "Public GitHub repository" and "Commit (optional)", and a Submit button. Above it the page says how many of the 2 submissions an hour are left and when the next one frees up.
- "Your team's submissions" lists each one, newest first: its ID, its state, its score, when, and `repository @ commit` (12 characters of the hash). A submission that fails the dev check shows its differing rows and its log there.
- Every submission except the entered one has a "Pick as our final entry" button. **Never press it.** The best score is entered in the final without anyone choosing.
- Scores of two machines can differ by up to about 20%, the page says; runs under 20 s are timed 3 times and the median counts.
- Submissions close Sunday 2026-11-22 at 11:00 AM EST.
- After Submit the page says "Received. Your receipt is BB-…", the form empties, and the new submission heads the list as "Scoring" (first "Fetching"); it turns to "Scored" with its time without a reload. The counter above the form then reads "1 of 2 left this hour" and when the next one frees up (an hour after the submission).
- The form's fields take a value set directly (no typing needed); the commit field shows the end of a 40-character hash, which is all that fits.

## Submissions

Rows up to `fac219f` were read off the page on 2026-11-21; the user made them by hand. Rows after it are the unattended cycles'.

| When (EST) | ID | Commit | Result | Score | Missing / extra / differing rows | Note |
| --- | --- | --- | --- | --- | --- | --- |
| Fri 2026-11-20, 8:49 PM | BB-6F32C523 | `d698c87b14060adeeb2940552dab479a0ad85ce9` | Scored | 1.05 s | none | |
| Fri 9:35 PM | BB-11C83E8B | `8cb3e8e0a0360a83ac6c273ca0ce859ad80cea4f` | Scored | 0.99 s | none | |
| Fri 9:59 PM | BB-38921F8B | `6bbe143db79edca466dc7b773980ec45b2ac7e0e` | Scored | 0.98 s | none | |
| Fri 10:35 PM | BB-18D24A7C | `7f9bffe36278e04662689cb0a596e03f0cff1a74` | Scored | 0.96 s | none | |
| Fri 11:10 PM | BB-2AF57432 | `c24627f787beead6f4e68dd5056c64dbd9beedb7` | Scored | 0.97 s | none | |
| Fri 11:49 PM | BB-9603EAD8 | `c6b4e3e1f74c2fd49ad1a9667d832563a5188de1` | Scored | 0.82 s | none | |
| Sat 2026-11-21, 12:26 AM | BB-5C420AD1 | `7a5ddaf05a8aa81bd381832f3fff484c5ae6622a` | Scored | 0.80 s | none | |
| Sat 12:53 AM | BB-DEE36DC7 | `fac219fce1a16f657f26dacc27a532c5fed06d8b` | Scored | 0.78 s | none | Was the entry for the final until 3:23 AM |
| Sat 3:23 AM | BB-34D7EB6F | `8527c64796f2913b4ba12a14e46291c19d2fbad8` | Scored | 0.76 s | none | Submitted by the unattended cycle 4 (experiments 27 to 29). Scored by the time the page was looked at again, 8 minutes later. Entered in the final as the best score, by the page, without anyone choosing. 15th of 208 teams then; the board's best was 0.42 s. Was the entry for the final until 6:50 AM |
| Sat 6:50 AM | BB-B16F12E9 | `e6ccab26927b27db7f100f68e403caf7e501854e` | Scored | 0.73 s | none | Submitted by the unattended cycle 9 (experiments 30 to 34; the code is `026badd`). "Scoring" 35 seconds after Submit, "Scored" 70 seconds after it. Entered in the final as the best score, by the page, without anyone choosing. 18th of 241 teams then; the board's best was 0.31 s. Was the entry for the final until 9:08 AM |
| Sat 9:08 AM | BB-83224571 | `252674a6f70b51a19e20b26eb9ca9a0485ed95fe` | Scored | 0.69 s | none | Submitted by the unattended cycle 13 (experiments 35 to 38). Still "Scoring" 55 seconds after Submit, "Scored" when looked at again 13 seconds later. Entered in the final as the best score, by the page, without anyone choosing. 19th of 260 teams then; the board's best was 0.31 s, the next four 0.36 to 0.54 s. Was the entry for the final until 10:13 AM |
| Sat 10:13 AM | BB-EE591719 | `8bde2f15b934df82545292ee715180824041f298` | Scored | **0.68 s** | none | Submitted by the unattended cycle 15 (experiments 39 and 41). "Scoring" ("Fetching") 5 seconds after Submit, "Scored" when looked at again 38 seconds later. Entered in the final as the best score, by the page, without anyone choosing. 21st of 269 teams; the board's best is 0.31 s (two teams), the next five 0.42 to 0.58 s. The page then read "1 of 2 left this hour", the next one free at 11:13 AM |

## Not submitted

- The probe (branch `probe`, `1e977ba52d396ec43d364a08456eb34688981470` since Sat 2026-11-21, 12:40 PM EST: `main` up to experiment 43 merged in, and the whole pipeline timed at 20M on the scorer's machine; it runs for about 40 s). It fails the dev check on purpose, so it never meets the conditions for an unattended submission; spending a slot on it is the user's call.
