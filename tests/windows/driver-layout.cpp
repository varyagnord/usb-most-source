// Validate the Rust byte layout against the pinned vendor C++ declarations.
#include <cstdio>
#include "../../third_party/usbip-win2/include/usbip/vhci.h"
using plugin = usbip::vhci::ioctl::plugin_hardware;
static_assert(sizeof(plugin) == 1124);
static_assert(offsetof(plugin, port) == 4);
static_assert(offsetof(plugin, location_hash) == 8);
static_assert(offsetof(plugin, busid) == 12);
static_assert(offsetof(plugin, service) == 44);
static_assert(offsetof(plugin, host) == 76);
static_assert(offsetof(plugin, serial) == 1104);
static_assert(offsetof(plugin, wsk_events) == 1120);
static_assert(sizeof(usbip::vhci::ioctl::plugout_hardware) == 8);
static_assert(usbip::vhci::ioctl::PLUGIN_HARDWARE_ONCE == 0x22e018);
static_assert(usbip::vhci::ioctl::PLUGOUT_HARDWARE == 0x22e004);
using imported = usbip::vhci::imported_device;
using imported_list = usbip::vhci::ioctl::get_imported_devices;
static_assert(sizeof(imported) == 1132);
static_assert(sizeof(imported_list) == 1136);
static_assert(offsetof(imported_list, devices) == 4);
static_assert(offsetof(imported, port) == 0);
static_assert(offsetof(imported, busid) == 8);
static_assert(offsetof(imported, service) == 40);
static_assert(offsetof(imported, host) == 72);
static_assert(usbip::vhci::ioctl::GET_IMPORTED_DEVICES == 0x22e008);
int main() { std::puts("PASS pinned MSVC driver ABI: plugin1124, imported1132/list1136, IOCTLs and offsets"); }
