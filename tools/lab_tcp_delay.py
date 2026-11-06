#!/usr/bin/env python3
"""Bounded, byte-preserving latency proxy between owned loopback test sockets."""
import argparse, asyncio, json, signal
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


async def relay(reader, writer, direction, count, delay_ms, jitter_ms, mode):
    """Preserve byte order; pipeline propagation delay with bounded backpressure.

    Serial mode intentionally preserves the older adversarial shaper: sleeping
    before every next read also caps throughput at roughly16KiB/delay. Pipeline
    mode overlaps propagation of queued chunks, without pretending to simulate
    packets, loss, congestion control or a physical link's bandwidth.
    """
    assert mode in ('serial', 'pipeline')

    def delay(step):
        jitter = 0 if not jitter_ms else ((step * 37) % (2*jitter_ms+1))-jitter_ms
        return (delay_ms+jitter)/1000

    if mode == 'serial':
        step = 0
        while data := await reader.read(16384):
            await asyncio.sleep(delay(step))
            writer.write(data); await writer.drain()
            count[direction] += len(data); step += 1
        writer.write_eof()
        return

    # At most8 queued chunks plus one in each task; StreamReader/transport
    # limits are bounded separately by the accepting socket adapter.
    pending = asyncio.Queue(maxsize=8)
    loop = asyncio.get_running_loop()

    async def receive():
        step, previous = 0, loop.time()
        while data := await reader.read(16384):
            due = max(previous, loop.time()+delay(step))
            await pending.put((due, data))
            previous = due; step += 1
        await pending.put(None)

    async def deliver():
        while item := await pending.get():
            due, data = item
            await asyncio.sleep(max(0, due-loop.time()))
            writer.write(data); await writer.drain()
            count[direction] += len(data)
        writer.write_eof()

    async with asyncio.TaskGroup() as group:
        group.create_task(receive())
        group.create_task(deliver())


async def main(a):
    stop = asyncio.Event(); jobs = set(); count = [0, 0]
    asyncio.get_running_loop().add_signal_handler(signal.SIGTERM, stop.set)
    async def accepted(reader, writer):
        task = asyncio.current_task()
        if len(jobs) >= 32:
            writer.close(); await writer.wait_closed(); return
        jobs.add(task); upstream = None
        try:
            remote, upstream = await asyncio.wait_for(asyncio.open_connection('127.0.0.1', a.target_port, limit=65536), 5)
            writer.transport.set_write_buffer_limits(high=65536)
            upstream.transport.set_write_buffer_limits(high=65536)
            async with asyncio.TaskGroup() as group:
                group.create_task(relay(reader, upstream, 0, count, a.delay_ms, a.jitter_ms, a.mode))
                group.create_task(relay(remote, writer, 1, count, a.delay_ms, a.jitter_ms, a.mode))
        except (Exception, asyncio.CancelledError): pass
        finally:
            for stream in (writer, upstream):
                if stream is not None:
                    stream.close()
                    try: await stream.wait_closed()
                    except (Exception, asyncio.CancelledError): pass
            jobs.discard(task)
    server = await asyncio.start_server(accepted, '127.0.0.1', 0, limit=65536)
    a.output.write_text(json.dumps({'port':server.sockets[0].getsockname()[1], 'delay_each_way_ms':a.delay_ms,'jitter_ms':a.jitter_ms,'mode':a.mode}))
    await stop.wait(); server.close(); await server.wait_closed()
    pending = list(jobs)
    for task in pending: task.cancel()
    await asyncio.gather(*pending, return_exceptions=True)
    a.output.with_suffix('.counts.json').write_text(json.dumps({'bytes_each_way':count,'scope':'encrypted byte counts only; no payload'}))

if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--target-port', type=int, required=True)
    p.add_argument('--delay-ms', type=int, required=True)
    p.add_argument('--jitter-ms', type=int, default=0)
    p.add_argument('--mode', choices=('serial', 'pipeline'), default='serial')
    a = p.parse_args()
    assert a.output.resolve().is_relative_to(ROOT/'build/e2e')
    assert 1024 <= a.target_port <= 65535 and 0 <= a.jitter_ms <= a.delay_ms <= 2000
    asyncio.run(main(a))
