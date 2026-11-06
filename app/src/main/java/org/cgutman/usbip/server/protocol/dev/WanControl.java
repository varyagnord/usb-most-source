package org.cgutman.usbip.server.protocol.dev;
import java.io.*;
import java.nio.ByteBuffer;
import org.cgutman.usbip.utils.StreamUtils;
public final class WanControl extends UsbIpDevicePacket {
  public static final int PUSH=16, START=17, CREDIT=18, BARRIER=19;
  public final int value, epoch;
  private WanControl(byte[] header,int value,int epoch){super(header);this.value=value;this.epoch=epoch;}
  public static WanControl read(byte[] header,InputStream in)throws IOException {
    byte[] rest=new byte[28];StreamUtils.readAll(in,rest);ByteBuffer b=ByteBuffer.wrap(rest);
    int value=b.getInt(4),epoch=b.getInt(8);
    if (ByteBuffer.wrap(header).getInt(4)!=0 || ByteBuffer.wrap(header).getInt(12)!=0
        || ByteBuffer.wrap(header).getInt(16)!=3 || b.getInt(0)!=0 || b.getInt(12)!=0
        || b.getLong(16)!=0 || b.getInt(24)!=0 || epoch<=0 || value<0
        || (ByteBuffer.wrap(header).getInt()==START && value!=0))throw new IOException("wan_control");
    return new WanControl(header,value,epoch);
  }
  protected byte[] serializeInternal(){throw new UnsupportedOperationException();}
  public static byte[] event(int command,int deviceId,int sequence,int epoch,int status,byte[] data) {
    int length=data==null?0:data.length;
    if(length>32 || epoch<=0)throw new IllegalArgumentException("wan_event");
    ByteBuffer b=ByteBuffer.allocate(48+length);
    b.putInt(command).putInt(sequence).putInt(deviceId).putInt(1).putInt(3)
      .putInt(status).putInt(length).putInt(epoch).putInt(0).putInt(0).putLong(0);
    if(data!=null)b.put(data);return b.array();
  }
}
