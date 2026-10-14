#!/usr/bin/env bash
# Installs build/<id>_<version>_arm.ipk on a Developer Mode TV over its ssh (port 9922, user prisoner).
#   GFN_TV=192.168.1.15 [GFN_TV_KEY=~/.ssh/lgtv_webos] scripts/tv-install.sh [--launch]
# The key is the Developer Mode app's (http://<tv>:9991/webos_rsa, unlocked with its passphrase).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tv="${GFN_TV:?set GFN_TV to the TV address}"
key="${GFN_TV_KEY:-$HOME/.ssh/lgtv_webos}"
id="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["id"])' "$root/pkg/appinfo.json")"
ipk="$(ls "$root"/build/"$id"_*_arm.ipk)"

ssh_tv() {
    ssh -i "$key" -p 9922 -o HostKeyAlgorithms=+ssh-rsa -o PubkeyAcceptedAlgorithms=+ssh-rsa \
        -o ConnectTimeout=8 "prisoner@$tv" "$@"
}

ssh_tv "cat > /media/developer/temp/$id.ipk" < "$ipk"
# The dev shell gets no tty and `luna-send-pub -n N` returns before appinstalld answers, so keep the
# subscription open in the background and read what it reported.
ssh_tv "luna-send-pub -i luna://com.webos.appInstallService/dev/install \
    '{\"id\":\"com.ares.defaultName\",\"ipkUrl\":\"/media/developer/temp/$id.ipk\",\"subscribe\":true}' \
    > /tmp/$id-install.log 2>&1 & p=\$!
  for _ in \$(seq 60); do sleep 1; grep -q -E '\"state\": \"(installed|install failed)\"|errorText' /tmp/$id-install.log && break; done
  kill \$p 2>/dev/null; rm -f /media/developer/temp/$id.ipk
  grep -E '\"state\"|errorText' /tmp/$id-install.log | uniq | tail -1"

if [ "${1:-}" = "--launch" ]; then
    ssh_tv "(luna-send-pub -i luna://com.webos.applicationManager/closeByAppId '{\"id\":\"$id\"}' >/dev/null 2>&1 & p=\$!; sleep 2; kill \$p 2>/dev/null)
      (luna-send-pub -i luna://com.webos.applicationManager/launch '{\"id\":\"$id\"}' >/dev/null 2>&1 & p=\$!; sleep 3; kill \$p 2>/dev/null)
      echo launched"
fi
