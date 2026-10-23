# Deploying the sync server

Relay's desktop application needs none of this. A sync server is only
for putting a workspace on more than one machine, or sharing it with
other people.

Two routes below. They produce the same server; what differs is who
runs the machine and what it costs.

## What it needs

- **Somewhere to run one container.** 512 MB is plenty.
- **A database.** SQLite on a persistent volume, or PostgreSQL.
- **A hostname**, reachable over TLS.
- **Optionally a mail provider.** Without one, confirmation and
  invitation links are written to the log instead of sent, which is a
  perfectly reasonable way to run a server for people you can talk to.

It needs **no object storage**. Nothing in Relay writes a blob: request
file uploads stream from the sender's own disk and never reach the
server, response bodies spool locally, and everything synchronised is
JSON in the database.

## Render, free

The free instance type has no persistent disk, so this route requires
PostgreSQL — the file would vanish on every restart. Neon's free tier
suits it, and between the two the deployment costs nothing.

1. Create a PostgreSQL database and copy its connection string. On Neon,
   make a database of Relay's own rather than sharing one.
2. In Render: **New → Blueprint**, and point it at this repository.
   `render.yaml` describes the service; Render will ask for the values
   it deliberately does not store in the repository.
3. Set them:

   | Variable | Value |
   |---|---|
   | `RELAY_DATABASE_URL` | the PostgreSQL connection string |
   | `RELAY_PUBLIC_URL` | `https://<your-service>.onrender.com` |
   | `RELAY_ADMIN_EMAIL` | the first administrator |
   | `RELAY_ADMIN_PASSWORD` | a long passphrase |
   | `RESEND_API_KEY` | optional; links go to the log without it |
   | `RELAY_MAIL_FROM` | an address on a domain verified with Resend |

4. Deploy, then check `https://<your-service>.onrender.com/health`.
5. Clear `RELAY_ADMIN_EMAIL` and `RELAY_ADMIN_PASSWORD` once the
   account exists. They are ignored after the first boot, but there is
   no reason to leave a password in an environment.

### What the free instance costs you

It sleeps after about fifteen minutes with no traffic. Relay tolerates
that better than most applications — the client queues its edits in an
outbox and the live-update socket reconnects with backoff — so nothing
is lost. What you lose is immediacy: while it sleeps, a teammate's
change arrives on the next sync rather than within moments, and the
first request after a sleep waits for a cold start.

Fine for trying it out or for one person on two machines. For a team
that relies on it, use the next section.

## A VM you control

Always on, no sleep, a few euros a month. Anything that runs Docker
will do.

1. Point an `A` record at the machine, and open ports 80 and 443.
2. On the machine:

   ```sh
   git clone https://github.com/l-robinsonl/relay && cd relay
   export RELAY_DOMAIN=sync.example.com
   export RELAY_ADMIN_EMAIL=you@example.com
   export RELAY_ADMIN_PASSWORD='a long passphrase'
   docker compose -f deploy/compose.prod.yml up -d
   ```

Caddy obtains the certificate on first start and renews it
thereafter — the reason it is there rather than nginx. The server is
not published on the host; only Caddy can reach it.

SQLite in a volume is the default and is a good answer: one file, one
backup, no second process. Set `RELAY_DATABASE_URL` to a PostgreSQL URL
when you would rather have replication or a managed database.

### Backing it up

```sh
docker compose -f deploy/compose.prod.yml exec relay \
  sqlite3 /data/relay.db ".backup /data/backup.db"
```

Then copy `backup.db` off the machine. With PostgreSQL, use whatever
your provider offers; Neon keeps point-in-time history of its own.

## What you do not need

One web service. That is the whole deployment.

- **No background worker.** There is no job queue. Mail is sent inside
  the request that causes it, migrations run at boot, and nothing is
  deferred.
- **No cron job.** Expired tokens and accepted invitations are swept
  by the next write that touches the same user or workspace, so the
  tables stay bounded without anything scheduled.
- **No Redis or key-value store.** Sessions are rows in the database.
- **No object storage.** Nothing in Relay writes a blob.

## Getting it wrong in ways that are quiet

The server warns at startup about each of these, because none of them
fails loudly on its own.

- **`RELAY_PUBLIC_URL` not matching the address people use.** It is
  what goes into every e-mailed link. Wrong, and the server works
  perfectly while nobody can accept an invitation.
- **`RELAY_REQUIRE_VERIFIED_EMAIL` on with no mail provider.** Links
  only reach the log, so nobody but the bootstrap administrator can
  sign in.
- **A proxy that closes idle connections.** A sync socket is idle most
  of the time by design; a short timeout turns live updates into a
  reconnect loop. The supplied `Caddyfile` disables those timeouts.
- **`RELAY_SIGNUP_MODE=open` on a server anyone can reach.** Use
  `invite` unless you mean it.
