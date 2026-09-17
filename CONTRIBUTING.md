# Contributing to kobo-backup

kobo-backup exists so that people never have to trust a backup tool blindly.
Every change is held to that standard: the user must be able to see what the
tool is about to do, confirm it, and verify it did what it said.

## Before you start

- **Bugs** are welcome as issues. Include your Kobo model, firmware version, and
  the on-screen output from the step that failed.
- **Features and behaviour changes** need discussion first. Open an issue and
  wait for a maintainer to approve the direction before writing code.
- **Security issues** must not be filed as public issues. See
  [SECURITY.md](.github/SECURITY.md).

Please read the [Code of Conduct](CODE_OF_CONDUCT.md).

## Issue first, pull request second

> Every pull request must link to an approved issue that is assigned to you.
> Pull requests without a linked, approved issue will be closed without review.

Approval means a maintainer has either commented approving the approach or
assigned the issue to you. Silence or a reaction is not approval. If nobody
responds within seven days, leave one polite follow-up and mention
@seamus-sloan or @roberte777.

Contributors with push access work on branches in this repository and may merge
their own pull requests once every check is green and every review thread is
resolved.

## Development

```bash
cargo run --release
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

Run that quality gate before opening a pull request.

### Changes that touch backup or restore

Anything on the path that reads from or writes to a device must keep the
safety model intact, as described in the
[README](README.md#safety-model): show the plan, confirm with the safe action
as the default, verify by re-reading what was written. A change to that path
also needs the
[manual verification protocol](README.md#manual-verification-protocol-real-hardware)
run against real hardware. Say in the pull request that you ran it and what
device you used.

## Pull requests

- **Commits and PR titles** use [Conventional Commits](https://www.conventionalcommits.org/)
  with `feat:`, `fix:`, or `chore:` and no scope. The PR title becomes the
  squash-merge subject.
- **Link the issue** with `Closes #<n>` in the body.
- **Copilot reviews every pull request.** Address or answer each of its
  comments and resolve the thread; merging requires all threads resolved.
- **Do not force-push during review** unless a reviewer asks. Push additional
  commits instead, and let human reviewers resolve their own threads.

Pull requests are squash-merged. A maintainer merges outside contributions
after review.

## License

kobo-backup is [MIT licensed](LICENSE). By contributing you agree that your
contributions are licensed under the same terms.
