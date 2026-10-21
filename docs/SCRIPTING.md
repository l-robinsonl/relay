# Scripting

A request can carry two scripts, written under its **Scripts** tab.

A **pre-request** script runs before the request goes out. It is for
producing something the request needs: a signature, a timestamp, a nonce.

```javascript
const stamp = String(Date.now())
relay.setHeader('X-Timestamp', stamp)
relay.setHeader('X-Signature', `${request.method}:${request.url}:${stamp}`)
```

A **post-response** script runs after the response arrives. It is for
asserting things about what came back, and for capturing a value — a
token, an id — for the next request to use.

```javascript
test('it returns the user', () => {
  expect(response.status).toBe(200)
  expect(response.json().name).toBe('Ada')
})
```

Results appear under **Tests**, beside the response. A failing assertion
never fails the request: the response is what you asked for, and the
verdict on it is separate information.

A pre-request script is different in one respect: if it throws, the
request is **not** sent. It usually exists to produce something the
request cannot do without, and sending anyway would turn a clear script
error into a puzzling 401.

## The sandbox

Scripts run in QuickJS, chosen for two reasons. It has no JIT, so nothing
generates executable pages at runtime. And its core has no I/O at all:
there is no `fetch`, no `require`, no filesystem, no process — not
because they are blocked, but because nothing ever put them there.

What a script can reach is a fixed set of plain data, passed in as JSON.
It holds no handle back into the application.

Three limits apply:

| | Default |
|---|---|
| Wall-clock deadline | 5 seconds |
| Heap | 64 MiB |
| Stack | 1 MiB |

A script that loops forever is interrupted. One that allocates without
bound is stopped. Neither takes the application with it, and the response
survives either way.

Console output is bounded too — 200 lines of 4 KiB — because a debugging
aid that can exhaust memory is not an aid.

### What a script cannot do

- Read or write a file
- Open a network connection
- Read an environment variable or reach the process
- Read a **secret** variable, or the `Authorization` header that was sent
- Overwrite a secret variable
- See anything left behind by a previous script

Each of these has a test. They matter more than the feature tests: a
sandbox that runs assertions correctly but leaks a credential is worse
than no sandbox, because it looks safe.

## What is available

### `response`

```javascript
response.status        // 200
response.statusText    // 'OK'
response.ok            // true for 2xx
response.headers       // { 'content-type': 'application/json' }
response.header(name)  // case-insensitive lookup
response.body          // the body as text
response.json()        // parsed; throws if the body is not JSON
response.time          // milliseconds
response.size          // bytes
response.url           // after any redirects
```

A binary body, or one large enough to have spilled to disk, arrives as an
empty string. Base64 in an assertion helps nobody.

### `request`

```javascript
request.method
request.url
request.headers        // credentials already redacted
```

### `relay`

```javascript
relay.getVariable('baseUrl')        // non-secret variables only
relay.setVariable('userId', '42')   // written back to the workspace
```

A variable a script sets becomes an ordinary workspace variable. It never
becomes a secret, and it cannot overwrite one — that would be a way round
the credential store rather than a feature.

### Assertions

```javascript
expect(x).toBe(y)              // ===
expect(x).toEqual(y)           // deep equality
expect(x).toBeDefined()
expect(x).toBeUndefined()
expect(x).toBeNull()
expect(x).toBeTruthy()
expect(x).toBeFalsy()
expect(x).toContain(y)         // string or array
expect(x).toHaveLength(n)
expect(x).toBeGreaterThan(n)
expect(x).toBeLessThan(n)
expect(x).toMatch(/pattern/)
expect(x).toHaveProperty('name')
expect(x).not.toBe(y)          // .not inverts any of them
```

`test(name, fn)` groups them. A failing assertion stops its own group at
the first failure — usually what was meant — and the groups after it
still run.

### Async

A test body may be `async`:

```javascript
test('eventually', async () => {
  const value = await Promise.resolve(response.json().id)
  expect(value).toBe(42)
})
```

Bodies are queued and awaited one at a time, which is what keeps an
assertion after an `await` attached to the right test. An earlier
implementation ran them inline; everything after the first `await` was
filed under no test at all, and the test reported a pass having asserted
nothing. A silently passing test is the worst failure mode a test runner
has, so it has a test of its own.

A promise that rejects with nothing to handle it is reported as a script
error rather than ignored.

### `console`

`console.log`, `.info`, `.debug`, `.warn` and `.error` are captured and
shown under Tests. Objects are rendered as JSON.

## Changing the request

These are available in a pre-request script only. In a post-response
script they throw, rather than quietly doing nothing to a request that
has already been sent.

```javascript
relay.setHeader(name, value)      // adds, or overwrites by name
relay.removeHeader(name)          // case-insensitive, as HTTP is
relay.setQueryParam(name, value)
relay.removeQueryParam(name)
relay.setUrl(url)
relay.setMethod(method)
relay.setBody(text)
```

`request` holds the request as it currently stands, with its variables
already resolved — so `request.url` is the URL that will actually be
requested, not `{{baseUrl}}/users`. The `Authorization` header is
redacted there, as it is everywhere else: a script may build a credential
but never read one.

Changes are applied **after** variables are resolved, so what you write
is what is sent. `relay.setHeader('X-Key', '{{apiSecret}}')` sends the
literal text `{{apiSecret}}`, not the secret. The same is true of a
variable a pre-request script sets: its value is substituted as written
and not expanded again. Both are deliberate. A script is not allowed to
read a secret, so it must not be able to name one and have Relay fetch it
on the script's behalf.

### Variables from a pre-request script

```javascript
relay.setVariable('nonce', crypto.randomUUID())
```

A variable set here applies to **this send only** and is then forgotten.
A pre-request script runs on every send, so persisting its output would
churn the workspace and push a revision per request. To keep a value,
capture it in the post-response script of whichever request produced it —
those are written back as ordinary workspace variables.

## Not supported

- **Modules.** `import` and `require` resolve to nothing.
- **Timers.** No `setTimeout`; a script runs to completion.
- **Chaining requests.** A script cannot send one. Capture a value into a
  variable and let the next request use it.
