import java.nio.ByteBuffer;
import java.util.*;
import java.util.concurrent.*;
import org.cgutman.usbip.jni.NativeManagement;

/** Actual bounded Rust/JNI registry. Synthetic metadata only; no USB or network. */
public final class ManagementJniRegressionTests {
  private static int checks;
  private static final byte[] EMPTY = new byte[0];
  private static void check(boolean ok, String label) {checks++; if (!ok) throw new AssertionError(label);}
  private static byte[] call(long id, int action, byte[] bytes) {return NativeManagement.execute(id, action, bytes);}
  private static byte[] report(byte[] key, long sequence, int state) {
    return ByteBuffer.allocate(40).put(key).putLong(sequence).putInt(state).putInt(0).array();
  }
  public static void main(String[] args) throws Exception {
    check(NativeManagement.create(null)==0,"null nonce");
    for(int n=0;n<=65;n++) {
      byte[] nonce=new byte[n];Arrays.fill(nonce,(byte)7);
      if(n!=16) check(NativeManagement.create(nonce)==0,"nonce dimensions");
    }
    check(NativeManagement.create(new byte[16])==0,"zero nonce");
    byte[] nonce=new byte[16];Arrays.fill(nonce,(byte)9);
    List<Long> models=new ArrayList<>();
    for(int i=0;i<8;i++) {long model=NativeManagement.create(nonce);check(model>0 && !models.contains(model),"unique model");models.add(model);}
    check(NativeManagement.create(nonce)==0,"bounded models");
    long stale=models.remove(0);NativeManagement.close(stale);NativeManagement.close(stale);
    check(call(stale,0,EMPTY)==null,"closed model rejected");
    long model=NativeManagement.create(nonce);models.add(model);check(model>stale,"handle is not reused");
    byte[] caps=call(model,0,EMPTY);check(caps.length==32,"caps size");check(ByteBuffer.wrap(caps).getInt()==0x554d4d31,"caps magic");
    check(Arrays.equals(Arrays.copyOfRange(caps,16,32),nonce),"session preserved");
    check(call(model,0,null)==null,"null input");check(call(model,0,new byte[53])==null,"oversize before allocation");
    check(call(model,-1,EMPTY)==null,"negative action");check(call(model,11,EMPTY)==null,"unknown action");
    byte[] request=new byte[52];request[0]='1';request[1]='-';request[2]='7';System.arraycopy(nonce,0,request,36,16);
    check(Arrays.equals(call(model,1,request),request),"validated managed import");
    request[31]=1;check(call(model,1,request)==null,"hidden busid suffix");request[31]=0;
    request[36]^=1;check(call(model,1,request)==null,"replaced session");
    List<byte[]> keys=new ArrayList<>();
    for(int i=1;i<=4;i++){byte[] key=call(model,2,EMPTY);check(key.length==24 && ByteBuffer.wrap(key).getLong(16)==i,"monotonic import");keys.add(key);}
    check(call(model,2,EMPTY)==null,"four managed imports maximum");
    byte[] key=keys.get(0),saved=key.clone();
    check(call(model,5,key)==null,"request needs receiver ready");check(call(model,8,key)==null,"cannot acknowledge unconfirmed safe");
    check(Arrays.equals(call(model,10,key),new byte[]{0}),"not ready");
    byte[] idle=report(key,0,0);check(ByteBuffer.wrap(call(model,6,idle)).getInt(32)==0,"first poll waits");
    check(Arrays.equals(call(model,10,key),new byte[]{1}),"ready after receiver poll");
    check(ByteBuffer.wrap(call(model,5,key)).getLong()==1,"explicit sequence allocated once");
    check(call(model,5,key)==null,"no second in-flight operation");
    byte[] directive=call(model,6,idle);check(ByteBuffer.wrap(directive).getLong(24)==1 && ByteBuffer.wrap(directive).getInt(32)==1,"lost directive repeated with same sequence");
    check(call(model,6,report(key,2,2))==null,"unsolicited future safe rejected");
    check(call(model,9,key).length==0,"deadline metadata only");check(call(model,6,idle)==null,"uncertain is not a fresh execution");
    check(call(model,3,key).length==0,"USB EOF retains late result");
    byte[] safe=report(key,1,2);check(call(model,6,safe)!=null,"late OS safe accepted");check(Arrays.equals(call(model,7,key),safe),"exact confirmed state");
    check(call(model,6,report(key,1,3))==null,"safe cannot become busy");
    check(call(model,8,key).length==0,"safe acknowledged after adapter checks");
    check(ByteBuffer.wrap(call(model,6,safe)).getInt(32)==2,"lost acknowledgement repeats Retire");
    check(call(model,5,key)==null,"retired key cannot execute");check(Arrays.equals(key,saved),"JNI input not modified");
    byte[] replacement=call(model,2,EMPTY);check(ByteBuffer.wrap(replacement).getLong(16)==5,"ack frees bounded slot without key reuse");
    check(call(model,4,replacement).length==0 && call(model,4,replacement).length==0,"idempotent exact withdrawal");
    for(long id:models) NativeManagement.close(id);

    long concurrent=NativeManagement.create(nonce);
    ExecutorService pool=Executors.newFixedThreadPool(8);CountDownLatch started=new CountDownLatch(8),go=new CountDownLatch(1);
    List<Future<?>> workers=new ArrayList<>();
    try {
      for(int i=0;i<8;i++) workers.add(pool.submit(()->{
        started.countDown();go.await();
        for(int j=0;j<500;j++) {byte[] value=call(concurrent,0,EMPTY);if(value!=null && (value.length!=32 || value[16]!=9)) throw new AssertionError("cross-model data");}
        return null;
      }));
      check(started.await(5,TimeUnit.SECONDS),"workers ready");go.countDown();NativeManagement.close(concurrent);
      byte[] other=nonce.clone();Arrays.fill(other,(byte)11);long fresh=NativeManagement.create(other);
      try {for(Future<?> worker:workers){worker.get(15,TimeUnit.SECONDS);checks++;}check(call(concurrent,0,EMPTY)==null,"stale handle cannot enter fresh model");}
      finally{NativeManagement.close(fresh);}
    } finally {go.countDown();pool.shutdownNow();NativeManagement.close(concurrent);}
    System.out.println("PASS management JNI: "+checks+" checks; 4000 concurrent close/stale lookups");
  }
}
