#!/usr/bin/env python3
"""Owned-loopback fault fixture: cut connections, reject traffic, then recover.

Control and evidence are local files under build/e2e; no payload is recorded.
No host interface, route, firewall or system daemon is changed.
"""
import argparse
import asyncio
import json
from pathlib import Path
import signal

ROOT = Path(__file__).resolve().parents[1]


class FaultProxy:
    def __init__(self, target_port):
        if not 1024 <= target_port <= 65535:
            raise ValueError('Expected owned loopback target')
        self.target_port = target_port
        self.online = True
        self.jobs = set()
        self.accepted = self.opened = self.rejected = 0
        self.bytes = [0, 0]
        self.server = None

    async def start(self):
        self.server = await asyncio.start_server(self.accept, '127.0.0.1', 0, limit=65536)
        return self.server.sockets[0].getsockname()[1]

    def snapshot(self):
        return dict(online=self.online, active=len(self.jobs), accepted=self.accepted,
                    upstream_opened=self.opened, rejected=self.rejected,
                    bytes_each_way=list(self.bytes))

    async def set_online(self, value):
        self.online = value
        if not value:
            pending = list(self.jobs)
            for job in pending:
                job.cancel()
            await asyncio.gather(*pending, return_exceptions=True)

    async def close(self):
        if self.server:
            self.server.close()
        # Python 3.13 waits for accepted connections in Server.wait_closed().
        # Cancel/abort them first, including sockets stranded by a route loss.
        await self.set_online(False)
        if self.server:
            await self.server.wait_closed()

    async def relay(self, reader, writer, direction):
        while data := await reader.read(16384):
            writer.write(data)
            await writer.drain()
            self.bytes[direction] += len(data)
        writer.write_eof()

    async def accept(self, reader, writer):
        self.accepted += 1
        if not self.online or len(self.jobs) >= 32:
            self.rejected += 1
            writer.transport.abort()
            return
        job = asyncio.current_task()
        self.jobs.add(job)
        upstream = None
        try:
            remote, upstream = await asyncio.wait_for(
                asyncio.open_connection('127.0.0.1', self.target_port, limit=65536), 5)
            self.opened += 1
            for stream in (writer, upstream):
                stream.transport.set_write_buffer_limits(high=65536)
            async with asyncio.TaskGroup() as tasks:
                tasks.create_task(self.relay(reader, upstream, 0))
                tasks.create_task(self.relay(remote, writer, 1))
        except (Exception, asyncio.CancelledError):
            pass
        finally:
            # A cut really discards this connection's queued bytes. Nothing is
            # kept for a later connection or replayed after recovery.
            writer.transport.abort()
            if upstream is not None:
                upstream.transport.abort()
            self.jobs.discard(job)


def write_json(path, value):
    temporary = path.with_name(path.name + '.next')
    temporary.write_text(json.dumps(value) + '\n')
    temporary.chmod(0o600)
    temporary.replace(path)


async def run(output, target_port):
    stop = asyncio.Event()
    asyncio.get_running_loop().add_signal_handler(signal.SIGTERM, stop.set)
    proxy = FaultProxy(target_port)
    port = await proxy.start()
    sequence = 0
    command_path = output.with_suffix('.command.json')
    state_path = output.with_suffix('.state.json')
    write_json(output, dict(port=port, scope='owned-loopback-fault-fixture'))
    try:
        while not stop.is_set():
            if command_path.exists():
                if command_path.is_symlink() or command_path.stat().st_size > 512:
                    raise ValueError('Invalid owned fixture command')
                command = json.loads(command_path.read_text())
                if (set(command) != {'sequence', 'online'}
                        or type(command['sequence']) is not int
                        or not 1 <= command['sequence'] <= 100
                        or type(command['online']) is not bool):
                    raise ValueError('Invalid owned fixture command')
                if command['sequence'] > sequence:
                    if command['sequence'] != sequence + 1:
                        raise ValueError('Out-of-order fixture command')
                    await proxy.set_online(command['online'])
                    sequence = command['sequence']
            write_json(state_path, dict(sequence=sequence, **proxy.snapshot()))
            try:
                await asyncio.wait_for(stop.wait(), .05)
            except TimeoutError:
                pass
    finally:
        await proxy.close()
        write_json(output.with_suffix('.counts.json'), proxy.snapshot())


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-port', type=int, required=True)
    args = parser.parse_args()
    output = args.output.absolute()
    if (output != output.resolve() or not output.is_relative_to(ROOT / 'build/e2e')
            or not output.parent.is_dir() or output.exists()):
        parser.error('Fresh output in an owned build/e2e directory required')
    asyncio.run(run(output, args.target_port))
