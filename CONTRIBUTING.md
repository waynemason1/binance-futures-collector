# Contributing

**Status: published as-is.** This is a personal project, shared because the code
may be useful to someone. It is not actively maintained, there is no roadmap, and
there is no commitment to respond to issues or review pull requests. Please read
that as honesty rather than hostility: it is better to say so than to leave an
issue sitting unanswered for a year.

You are free to fork it. The MIT licence means you can take it in whatever
direction you like without asking.

## If you do open something

Issues and pull requests are read when time allows. They may not be. A fork is
often the faster route to what you want.

For anything larger than a bug fix, open an issue before writing code, so you
don't spend effort on something that would be declined on scope grounds.

## Scope

Deliberately narrow: **raw capture only**. Signals, feature engineering,
backtesting and trading logic are out of scope. An earlier version of this
codebase carried all of that and it was removed on purpose.

Exchanges other than Binance USDT-M futures are also out of scope. The order-book
continuity handling is specific to Binance's diff-stream semantics, and a generic
abstraction would cost the correctness guarantees that are the point of the
project.

## Running it locally

```bash
./setup.sh                      # interactive: Docker or native, data dir, pairs, cadence
docker compose up -d --build    # or straight to containers
```

## What CI checks

If you do open a PR, this is what has to pass, and running it locally first saves a
round trip:

Run all of it from the repo root; the frontend steps use a subshell so you stay
there. The backend tests need their dev dependencies first, on Python 3.10 or
later — macOS ships 3.9 as the default `python3`, and the tests fail on it:

```bash
python3.13 -m venv .venv && source .venv/bin/activate
pip install -r dashboard/backend/requirements.txt -r dashboard/backend/requirements-dev.txt
```

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release --locked
cargo test --all

python -m pytest dashboard/backend/tests -q
(cd dashboard/frontend && npm ci && npm run build && npm test)
docker compose build

# the security job, which is easy to forget and will fail CI on its own
cargo install cargo-audit --locked && cargo audit
(cd dashboard/frontend && npm audit --omit=dev --audit-level=high)
```

Clippy runs with `-D warnings`, so a warning fails the build. The frontend needs
Node 24 LTS or newer (npm 11+); npm 10 mis-reads this lockfile's optional peer
dependencies and refuses `npm ci`.

## House style

Match the surrounding code rather than importing your own conventions. Comments
here explain *why* something is the way it is: a Binance quirk, a measured
result, a constraint that isn't obvious from the code. A comment that only
restates the line below it should be deleted rather than reworded.

Claims about behaviour should be checkable. If you change something the README or
`docs/` describes numerically (throughput, memory, disk rates), update the number
and say how you measured it; `docs/SOAK_TEST.md` is the model for that.

Bug fixes should come with a test that fails before the fix. The validators in
particular are tested against seeded corruption, so if you touch them, extend that
rather than trusting a clean run.

## Reporting bugs

Include the commit, your OS, whether you are on the native or Docker path, the
relevant part of `config.toml` with any key redacted, and the log lines around the
failure. For data-quality problems, the Validation tab's output for the affected
day is the most useful thing you can attach.

Security issues go to [SECURITY.md](SECURITY.md), not the issue tracker.
