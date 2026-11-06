import java.net.*;
import java.io.*;
import java.nio.ByteBuffer;
import java.util.*;
import org.cgutman.usbip.server.*;
import org.cgutman.usbip.server.protocol.*;
import org.cgutman.usbip.server.protocol.dev.*;
public final class WanProtocolTests {
 static int checks;
 static void check(boolean b,String why){checks++;if(!b)throw new AssertionError(why);}
 static final class Handler implements UsbRequestHandler {
  int attaches,commands;boolean lastWan;UsbIpServer server;
  UsbDeviceInfo info=new UsbDeviceInfo();
  Handler(){info.dev=new UsbIpDevice();info.dev.path="/synthetic";info.dev.busid="1-6";info.dev.busnum=1;info.dev.devnum=6;info.dev.idVendor=0x046d;info.dev.idProduct=(short)0xc52b;info.interfaces=new UsbIpInterface[0];}
  public List<UsbDeviceInfo> getDevices(){return Collections.singletonList(info);}
  public UsbDeviceInfo getDeviceByBusId(String b){return info;}
  public UsbDeviceInfo getAttachedDevice(Socket s){return info;}
  public boolean attachToDevice(Socket s,String b){return attachToDevice(s,b,false);}
  public boolean supportsWan(String b){return b.equals("1-6");}
  public boolean attachToDevice(Socket s,String b,boolean wan){attaches++;lastWan=wan;return true;}
  public void detachFromDevice(Socket s,String b){}
  public void cleanupSocket(Socket s){}
  public void submitUrbRequest(Socket s,UsbIpSubmitUrb r){}
  public void abortUrbRequest(Socket s,UsbIpUnlinkUrb r){}
  public void wanControl(Socket s,WanControl c){commands++;server.send(s,WanControl.event(WanControl.PUSH,65542,commands,c.epoch,0,new byte[]{1,2,3}));}
 }
 static byte[] header(int op){return ByteBuffer.allocate(8).putShort((short)0x111).putShort((short)op).putInt(0).array();}
 static byte[] imported(int op,String bus,int profile){ByteBuffer b=ByteBuffer.allocate(op==0x80e0?44:40);b.put(header(op));byte[] id=new byte[32];System.arraycopy(bus.getBytes(java.nio.charset.StandardCharsets.US_ASCII),0,id,0,bus.length());b.put(id);if(op==0x80e0)b.putInt(profile);return b.array();}
 static Socket socket()throws IOException{Socket s=new Socket("127.0.0.1",3240);s.setSoTimeout(2000);return s;}
 static byte[] exact(Socket s,int n)throws IOException{byte[]b=new byte[n];new DataInputStream(s.getInputStream()).readFully(b);return b;}
 static void rejected(byte[] data)throws IOException{try(Socket s=socket()){s.getOutputStream().write(data);try{check(s.getInputStream().read()==-1,"invalid request closed");}catch(SocketException e){check(true,"invalid reset");}}}
 public static void main(String[] args)throws Exception{
  Handler h=new Handler();UsbIpServer server=new UsbIpServer(x->{});h.server=server;server.start(h,"127.0.0.1");
  try{
   try(Socket s=socket()){s.getOutputStream().write(header(0x80df));ByteBuffer b=ByteBuffer.wrap(exact(s,24));check(b.getShort()==0x111&&b.getShort()==0xdf&&b.getInt()==0,"capabilities header");check(b.getInt()==0x57414e31&&b.getInt()==2048&&b.getInt()==32&&b.getInt()==1,"capabilities bounds");}
   check(h.attaches==0,"capability probing never claims USB");
   try(Socket s=socket()){s.getOutputStream().write(imported(0x80e0,"1-7",1));ByteBuffer b=ByteBuffer.wrap(exact(s,8));b.position(4);check(b.getInt()==95,"unsupported profile refused before attach");}check(h.attaches==0,"unknown USB never optimized");
   for(int n:new int[]{0,2,-1,Integer.MAX_VALUE})rejected(imported(0x80e0,"1-6",n));
   byte[] bad=imported(0x80e0,"1-6",1);bad[39]=1;rejected(bad);bad=imported(0x80e0,"1-6",1);bad[1]=0;rejected(bad);
   try(Socket s=socket()){
    s.getOutputStream().write(imported(0x80e0,"1-6",1));byte[] b=exact(s,336);check(ByteBuffer.wrap(b).getInt(4)==0&&h.lastWan,"WAN import success");check(ByteBuffer.wrap(b).getInt(320)==0x57414e31,"negotiated capability suffix");
    byte[] start=ByteBuffer.allocate(48).putInt(17).putInt(0).putInt(65542).putInt(0).putInt(3).putInt(0).putInt(0).putInt(1).putLong(0).putLong(0).array();s.getOutputStream().write(start);
    check(ByteBuffer.wrap(exact(s,51)).getInt()==16,"negotiated push passes actual server writer");
    start[0]=0;start[3]=18;ByteBuffer.wrap(start).putInt(24,1);s.getOutputStream().write(start);exact(s,51);check(h.commands==2,"credit acknowledged only in WAN connection");
   }
   try(Socket s=socket()){s.getOutputStream().write(imported(0x8003,"1-6",0));exact(s,320);s.getOutputStream().write(ByteBuffer.allocate(48).putInt(17).array());try{check(s.getInputStream().read()==-1,"generic connection rejects WAN controls");}catch(SocketException e){check(true,"generic reset");}}
   check(h.commands==2,"generic traffic cannot start pump");
   try(Socket s=new Socket()){s.bind(new InetSocketAddress("127.0.0.2",0));s.connect(new InetSocketAddress("127.0.0.1",3240));s.setSoTimeout(2000);try{s.getOutputStream().write(header(0x80df));check(s.getInputStream().read()==-1,"wrong peer denied capabilities");}catch(SocketException e){check(true,"wrong peer reset");}}
  }finally{server.stop();}
  System.out.println("PASS WAN negotiation/capability isolation: "+checks+" checks");
 }
}
