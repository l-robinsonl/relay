# How to talk about this work

Read this before writing any reply in this project.

The person you are reporting to is building Relay and makes every real
decision about it. They are not reading your messages to admire the
engineering — they are reading them to find out **what changed, whether
that is good or bad, and whether they need to do anything.**

This is not a request to simplify the work. It is a request to stop
making them translate it.

## The rule

**Lead with what a person using Relay would notice. Then, if it still
matters, say how.**

Most of what has gone wrong has been the other way round: three
paragraphs of mechanism, with the consequence buried at the end or left
implicit. If the reader has to reach the second half of a sentence to
learn whether something is broken, the sentence is backwards.

## Say it this way instead

These are real sentences from this project, rewritten.

> Routing a provider refusal through `Internal` logs the literal string
> "internal error", so the variant keeps its detail in `Display` and
> still flattens on the wire.

**Better:** When Stripe rejected something, the error log just said
"internal error" and nothing else. If you had set up the wrong price,
you would have had no way to find out why. The log now includes
Stripe's own explanation. Users still see nothing.

---

> A seat limit answered 413 Payload Too Large rather than 402 Payment
> Required.

**Better:** When a workspace runs out of seats, the server replied with
an error that means "your request was too big" — confusing for anyone
reading the logs. It now says "payment needed", which is what is
actually true.

---

> A `<select>` bound with `:value` serialises without the `selected`
> attribute, so it photographs as its first option.

**Better:** My screenshots cannot show which option a dropdown has
chosen — every dropdown looks like it is set to the first one. So a
dropdown that looks wrong in a screenshot may well be fine. I check in
the real app before changing anything.

---

> `ipc.ts` hand-wrote four types that ts-rs already generates, and the
> copies had drifted.

**Better:** The same information was written down in two places — once
generated automatically from the Rust code, once typed out by hand. The
hand-typed copy had fallen out of date and was missing a field. Nothing
would have caught it. There is only one copy now.

## Words to stop using without explaining them

The left column is fine *once you have said what it means*, in half a
sentence, the first time it comes up in a conversation. Never assume it
carried over from last week.

| Instead of | Say |
|---|---|
| webhook | a message Stripe sends the server when something happens |
| HMAC / signature verification | proof the message really came from Stripe |
| idempotency / replay guard | making sure the same message twice does not count twice |
| entitlement | what the workspace has paid for |
| bindings / ts-rs | the shared list of data shapes, generated from the Rust code |
| drift | two copies of the same thing that no longer agree |
| conformance suite | the tests that run against the real database |
| clippy / typecheck / lint | automated checks for mistakes |
| CI | the service that builds and tests on every push |
| teleport / DOM property | (just describe what the user sees) |
| the `btn-danger` class | the red button style |
| 402 / 413 / 404 | what the error means, in words |

## The shape of a good update

1. **One line saying what state things are in.** Good news or bad, up
   front.
2. **What changed, in terms of what someone would notice.** Two or
   three short paragraphs at most.
3. **Anything you are unsure about or deliberately did not do.** Say
   it plainly. Do not bury it.
4. **What you need from them** — at the end, clearly marked, and
   ideally one thing. If there are three, say which one matters.

Do not open with a table. Do not list commit hashes unless asked. Do
not recap what was already agreed earlier in the conversation.

## Length

Shorter than feels right. If an update runs past about fifteen lines,
something in it is for your benefit rather than theirs.

A long explanation is not thoroughness, it is unsorted thinking handed
over for someone else to sort.

## What not to do in the name of plain English

- **Do not become vague.** "I improved the billing panel" is worse than
  jargon. Name the actual change and its actual effect.
- **Do not hide bad news in soft words.** If something is unproven, say
  "I have not tested this", not "this is pending verification".
- **Do not drop the detail entirely.** Offer it: "the short version is
  X — tell me if you want the detail." Then stop.
- **Do not explain what was not asked.** A question about one thing is
  not an invitation to summarise the project.
- **Do not be condescending.** This person reads the code and makes the
  calls. The problem was never their understanding; it was your
  packaging.

## When jargon is right

In code, comments, commit messages and `docs/`. Those are written for
whoever is in the code next, and precision beats accessibility there.

This document is about replies in conversation. Different audience,
different register.
