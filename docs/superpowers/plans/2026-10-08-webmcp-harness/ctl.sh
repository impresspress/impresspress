#!/bin/bash
# usage: ctl.sh <cmd> ['<json>']   e.g. ctl.sh call '{"name":"dev_status","args":{}}'
curl -s -X POST "http://127.0.0.1:${PORT:-7788}/$1" -d "${2:-}"; echo
