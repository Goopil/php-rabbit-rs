#!/usr/bin/env bash
# Round L profile — provenance snapshot for the measurement session.
# Records machine context next to the archived numbers: load average,
# xdebug absence under the `php -n` invocation, extension version, lab state.
set -euo pipefail
OUT="${1:?usage: provenance.sh <output.json>}"
{
  php -n -d extension="${RABBIT_RS_DYLIB:?}" -r \
    'echo json_encode(["php"=>PHP_VERSION,"rabbit_rs"=>phpversion("rabbit_rs"),"xdebug"=>extension_loaded("xdebug")]);'
} > "${OUT}"
LOAD="$(uptime | sed -E 's/.*load averages: //')"
python3 - "$OUT" "$LOAD" <<'PY'
import json, sys
path, load = sys.argv[1], sys.argv[2]
d = json.load(open(path))
d["xdebug_free_php_n"] = not d.pop("xdebug")
d["load_averages"] = [float(x) for x in load.split()]
json.dump(d, open(path, "w"), indent=2)
PY
cat "${OUT}"
