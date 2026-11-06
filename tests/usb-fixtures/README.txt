rutoken-lite-descriptors.bin: 111 bytes of cached public USB descriptors from
Aktiv Rutoken Lite 0a89:0025, read from Linux sysfs on 2026-11-01. Device descriptor
iSerialNumber is zero. No string descriptors, PIN, certificates, APDU or transfer
payload are included. Configuration wTotalLength=93, interface bNumEndpoints=2,
but three framed endpoints follow (02 bulk, 82 bulk, 81 interrupt). Linux accepts
this device with an endpoint-count warning. Regression tests must accept this
bounded firmware quirk without weakening framing, duplicate-address or ISO checks.
