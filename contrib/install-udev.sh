#!/bin/sh
# Installs the udev rule that gives group `dialout` access to the MHO954 (USB device node for
# the user-space transport and /dev/usbtmcN for the kernel driver). Run with sudo.
set -eu
here=$(dirname "$0")
install -Dm644 "$here/../udev/62-rigol-usbip.rules" /etc/udev/rules.d/62-rigol-usbip.rules
udevadm control --reload
udevadm trigger --subsystem-match=usb --attr-match=idVendor=1ab1
echo "udev rule installed. Make sure your user is in group dialout:  id -nG | grep -w dialout"
