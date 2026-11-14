#!/bin/sh
# Stand-in executable for tests that need a command with a script of their
# own. A test links the command's name to this file and writes the script as
# `<name>.body` next to the link; this file runs it with the same arguments
# and standard input, and `$0` inside the script is still the link, so
# `dirname "$0"` and `basename "$0"` answer as if the script were the command.
#
# It is a checked-in executable on purpose: a test that writes an executable
# and then runs it can fail with ETXTBSY ("Text file busy"), because a child
# forked by another test at that moment keeps the write handle open until its
# own exec. Nothing writes this file while the suite runs. The link is not
# resolved: the kernel hands the script the path it was invoked through.
exec /bin/sh -c '. "$0.body"' "$0" "$@"
