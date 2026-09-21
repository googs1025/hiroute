# Local configuration saves

HiRoute's owner-only Local Control socket admits processes running as the same OS user as
`hirouted`. Desktop and CLI use this one local-user scope for configuration saves and Operation
recovery. They do not need a Desktop/CLI-specific Apply capability or a second authorization
confirmation. A Save, Publish, or Use this installation click is the ordinary user intent.

The daemon still validates the exact normalized change, digest, resource revisions, idempotency
key, and durable writer admission. On a conflict, refresh the current state and submit the
intended edit again. If a response is lost, first query or replay the same operation identity;
do not invent a new key or assume that a missing response means failure.

This local trust decision does not authorize remote Agent collaboration, Gateway calls, or
potentially billable model checks. Those flows keep their own consent and credential boundaries.
The Desktop WebView can only use its explicitly exposed native commands and cannot supply an
arbitrary Local Control request.
