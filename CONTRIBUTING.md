# Contributing

This repository uses [vouch](https://github.com/mitchellh/vouch) for contributor trust management.
You must be vouched to open pull requests. Pull requests from unvouched users may be closed automatically.
Denounced users are always blocked.

## Getting Vouched

1. Open an issue describing what you want to change and why.
2. Keep the proposal concise and concrete.
3. A maintainer can vouch for you by commenting `vouch` on your issue.
4. Once vouched, you can open pull requests normally.

## Maintainer Commands

Maintainers can manage trust status in issue comments with:

- `vouch`
- `vouch @username <optional reason>`
- `denounce`
- `denounce @username <optional reason>`
- `unvouch`
- `unvouch @username`

These commands update `.github/VOUCHED.td` through GitHub Actions.
