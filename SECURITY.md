# Security

## Support status

This project is published as-is and is **not actively maintained**. There is no
security SLA, no guaranteed response time, and no commitment to issue patches or
backports. Assume you are responsible for reviewing and maintaining the code you
deploy. If that is not acceptable for your use case, fork it and own it; the MIT
licence exists for exactly that.

No version of this project is under active support:

| Version | Supported |
|---|---|
| all | :x: (published as-is) |

## Reporting a vulnerability

Open a [private security advisory](../../security/advisories/new) rather than a
public issue, so the details are not disclosed while anyone is still exposed.

Include what you found, how to reproduce it, and what an attacker gains. Reports
are read when time allows, but no response should be assumed. If you need a fix on
a timeline, fixing it in your own fork is the reliable path, and a pull request
alongside the report makes it far more likely to land here too.

## Scope and known limitations

The collector reads **public market data only**. It needs no account and no API
key, and it never places orders: there is no trading code in this repository.

Two things are known and by design:

**The dashboard has no authentication.** It is intended for `localhost` or a
trusted LAN, and `docker-compose.yml` binds it to `127.0.0.1` for that reason. The
Config tab writes `config.toml` and "Restart to apply" drops a sentinel the
collector obeys, so anyone who can reach the dashboard can reconfigure and
restart the collector. Do not expose it without putting authentication and TLS in
front of it. Reports that the dashboard is unauthenticated are **known**; reports
that it is reachable *despite* the localhost bind, or that it can be driven from a
malicious page, are in scope.

**An optional read-only API key.** If you set `api_key` in `config.toml` it is
sent as the `X-MBX-APIKEY` header on REST requests. It is not required for
anything the collector does. If you use one, enable **Reading only**, disable
trading and withdrawals, and IP-restrict it. `config.toml` is gitignored, so a
configured key is not committed by accident.

## Handling captured data

Output under `data/` is public market data, not secrets, but it is large and it is
gitignored for a reason. Do not commit it.
