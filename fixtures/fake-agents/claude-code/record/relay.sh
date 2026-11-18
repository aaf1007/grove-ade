#!/bin/sh
# Stub hook relay for recording: appends the hook's stdin to the file in $1,
# one payload per line. Like grove-hook it prints nothing and always exits 0.
{ cat; echo; } >>"$1"
exit 0
