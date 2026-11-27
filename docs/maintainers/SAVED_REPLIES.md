# Saved replies

Replies maintainers paste into issues, discussions and pull requests. Copy them into GitHub's
[saved replies](https://github.com/settings/replies) (per account) or paste them from here. Replace
every `<...>` before posting, and keep the tone short and friendly.

## Duplicate

Use when an open or closed issue already covers the same bug. Then close this one as "duplicate"
(Close > Close as duplicate) and add the `duplicate` label.

```markdown
Thanks for the report! This is the same bug as #<number>, so I'm closing this one as a duplicate.
Please follow #<number> for updates. If your case is different (another version, other steps, a
different result), comment there with the details; that helps more than a second issue.
```

## Feature request → Discussions > Ideas

Use when an issue asks for something new rather than reporting a bug. Prefer GitHub's
"Convert to discussion" (category Ideas) over closing, so the author keeps their text and votes.

```markdown
Thanks for the idea! Issues here are for bugs, and ideas live in
[Discussions > Ideas](https://github.com/murmaider227/rustgram/discussions/categories/ideas), where
others can vote and add to them. I've moved this there. If it turns out Telegram Desktop already
does this and Rustgram does not, that's a parity bug: reopen it as a bug report with the steps.
```

## Cannot reproduce

Use when you tried the exact version and steps and saw no problem. Name the version and the steps
you used, so the reporter can see where their setup differs. Then add `needs more info`
(see the next reply for the rule about the order).

```markdown
Thanks for the report. I could not reproduce this on Rustgram <version> (<installer / portable zip>,
Windows <version>) with these steps:

1. <step>
2. <step>
3. <step>

Result: <what I saw>.

Could you check:

- the exact Rustgram version (bottom of the main menu, or rustgram.exe > Properties > Details);
- whether it still happens on the [latest release](https://github.com/murmaider227/rustgram/releases);
- what you do differently from the steps above;
- the lines from `log.txt` around the time it happens (see
  [docs/REPORTING_BUGS.md](https://github.com/murmaider227/rustgram/blob/main/docs/REPORTING_BUGS.md)
  for where it is), with private data removed?
```

## Needs more info

Post the comment first, then add the `needs more info` label as the last action. Any update after
the label (including your own comment or another label) removes it again, and the issue goes back
to `needs triage`. With the label in place, the issue closes automatically after 7 days without a
reply, and a reply after that reopens nothing by itself: reopen it by hand when the details arrive.

```markdown
Thanks! To look into this I need a bit more:

- <the missing item, e.g. the exact version>
- <the missing item, e.g. steps that show the problem from a fresh start>

The issue closes automatically if there is no reply within 7 days, so nothing stays open forever.
Reply any time with the details and I'll reopen it.
```

## Pull request re-applied with credit

This repository is an export of a private repository, so pull requests are never merged here. Re-apply
the change there with the author as co-author, then close the PR with this reply and the
`re-applied` label. Add the trailer
`Co-authored-by: <name> <id+login@users.noreply.github.com>` (the author's GitHub noreply address,
from their profile or from the PR's commits) to the private commit **and** to the message of the
sync commit that publishes it here: only the public commit makes GitHub count it for the author.
One sync commit can carry several trailers.

```markdown
Thank you, @<login>! Pull requests can't be merged here directly (this repository is an export of
the one we build from), so I re-applied your change there with you as co-author. It reaches this
repository with the next sync, in a commit that names you as co-author<, or: in <commit link>>. Closing this PR now; the change ships in
the next release. See "Pull requests" in CONTRIBUTING.md for how this works.
```

If the change needed edits, add one line saying what was changed and why.

## Pull request not taken

```markdown
Thanks for the work on this! I'm not taking this change because <reason: it adds a feature that
should start in Discussions > Ideas / it changes behaviour away from Telegram Desktop / it mixes
several problems>. <What would help instead, if anything.> Closing for now.
```
