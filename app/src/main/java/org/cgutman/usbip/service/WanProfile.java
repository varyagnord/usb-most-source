package org.cgutman.usbip.service;
import android.hardware.usb.*;
/** Capability, not a generic HID-class heuristic: never speculate reads on tokens. */
final class WanProfile {
  static boolean receiver(UsbDevice d) {
    if(d.getVendorId()!=0x046d || d.getProductId()!=0xc52b || d.getConfigurationCount()!=1)return false;
    for(int c=0;c<d.getConfigurationCount();c++) {
      UsbConfiguration cfg=d.getConfiguration(c);
      for(int i=0;i<cfg.getInterfaceCount();i++) {
        UsbInterface f=cfg.getInterface(i);
        if(f.getId()!=2 || f.getAlternateSetting()!=0 || f.getInterfaceClass()!=3)continue;
        for(int j=0;j<f.getEndpointCount();j++) {
          UsbEndpoint e=f.getEndpoint(j);
          if(e.getAddress()==0x83 && e.getType()==3 && e.getMaxPacketSize()==32)return true;
        }
      }
    }
    return false;
  }
}
