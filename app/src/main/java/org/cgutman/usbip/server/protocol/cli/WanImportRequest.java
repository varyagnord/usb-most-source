package org.cgutman.usbip.server.protocol.cli;
import java.io.*;
import java.nio.ByteBuffer;
import org.cgutman.usbip.utils.StreamUtils;
/** Negotiation only; no profile can be enabled by a normal USB/IP client. */
public final class WanImportRequest extends ImportDeviceRequest {
  public static final short CODE=(short)0x80e0, CAPS=(short)0x80df;
  public WanImportRequest(byte[] header, InputStream in) throws IOException {
    super(header);
    if (version!=0x111 || status!=0) throw new IOException("wan_version");
    byte[] bus=new byte[32];StreamUtils.readAll(in,bus);
    int end=0;while(end<bus.length && bus[end]!=0)end++;
    if(end==bus.length)throw new IOException("wan_busid");
    for(int i=end;i<bus.length;i++)if(bus[i]!=0)throw new IOException("wan_busid_padding");
    busid=new String(bus,0,end,java.nio.charset.StandardCharsets.US_ASCII);
    byte[] p=new byte[4];StreamUtils.readAll(in,p);
    if (!busid.matches("[1-9][0-9]{0,5}-[1-9][0-9]{0,5}") || ByteBuffer.wrap(p).getInt()!=1)
      throw new IOException("wan_profile");
  }
  public static CommonPacket caps(byte[] header) throws IOException {
    return new CommonPacket(header) {
      { if(version!=0x111 || status!=0) throw new IOException("wan_version"); }
      protected byte[] serializeInternal(){return null;}
    };
  }
}
