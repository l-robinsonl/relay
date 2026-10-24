/**
 * The server Relay runs, offered as a convenience.
 *
 * Someone who does not run a server of their own has nothing to type
 * into the address field, and the dialog used to demand one before it
 * would show them a sign-in form at all. This is the default so they
 * have something to click; self-hosting sits beside it, not beneath
 * it, because the product does not work the other way round.
 *
 * One constant, one place to change it when the server moves.
 */
export const HOSTED_SERVER = 'https://relay-sync-ct0w.onrender.com'

/** What to call it in front of a person. */
export const HOSTED_NAME = 'Relay’s server'
