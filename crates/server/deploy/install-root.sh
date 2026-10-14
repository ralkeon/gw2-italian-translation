#!/bin/sh
# Installs the gw2it-sync service and its Apache proxy (https://ifalna.ralkeon.com/gw2it/).
# Run once: sudo sh ~/gw2it-sync/install-root.sh
set -e
here=/home/ralkeon/gw2it-sync
vhost=/etc/apache2/sites-available/ifalna.ralkeon.com-le-ssl.conf

install -m 644 "$here/gw2it-sync.service" /etc/systemd/system/gw2it-sync.service
systemctl daemon-reload
systemctl enable --now gw2it-sync
sleep 1
systemctl is-active gw2it-sync

if grep -q "ProxyPass        /gw2it/" "$vhost"; then
    echo "Apache: proxy already there"
else
    cp "$vhost" "$vhost.bak-gw2it"
    # Before the closing </VirtualHost>.
    sed -i "/<\/VirtualHost>/e cat $here/apache-gw2it.conf" "$vhost"
    if apache2ctl configtest; then
        systemctl reload apache2
        echo "Apache: proxy added (backup in $vhost.bak-gw2it)"
    else
        cp "$vhost.bak-gw2it" "$vhost"
        echo "Apache: configtest failed, vhost restored" >&2
        exit 1
    fi
fi
curl -s https://ifalna.ralkeon.com/gw2it/ && echo
