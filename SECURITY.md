# Security

Please don't open a public issue for a security problem. Report it privately through GitHub:
on the repository's **Security** tab, choose **Report a vulnerability** (a private security
advisory). Include what you found, how to reproduce it, and which version you used.

You'll get an answer as soon as possible. Fixes are released through the normal update channel,
and the advisory is published once a fixed version is out.

Areas of particular interest: handling of the OpenProject API key (kept in the system keyring),
the self-updater (signature checks), and anything that could make the app record more than the
README says it does.
