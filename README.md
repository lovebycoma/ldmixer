# lD Mixer

An unofficial, Flatpak-first mixer and control panel for Audient iD USB audio
interfaces on Linux. The name is a pun: **l**D Mixer = **L**inux iD Mixer.

Work in progress. Developed and tested first on an Audient iD24.

## USB access

lD Mixer talks to the interface over USB from user space, through the spare
DFU interface, so audio keeps playing and no kernel module is needed. Linux
does not let normal users open USB devices by default, and a Flatpak cannot
change that, so access must be granted once, by you, outside the app:

```sh
sudo install -m644 data/70-ld-mixer.rules /etc/udev/rules.d/
sudo udevadm control --reload
sudo udevadm trigger --subsystem-match=usb --attr-match=idVendor=2708
```

lD Mixer itself never changes system settings.
