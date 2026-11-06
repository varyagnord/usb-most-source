"""Actual loopback bytes and connection ownership; no Android or USB."""
import asyncio
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'tools'))
from lab_tcp_fault import FaultProxy


class FaultProxyTests(unittest.IsolatedAsyncioTestCase):
    async def test_cut_reject_and_recover_without_reusing_old_connection(self):
        messages = []
        jobs = set()

        async def echo(reader, writer):
            job = asyncio.current_task()
            jobs.add(job)
            try:
                while data := await reader.read(65536):
                    messages.append(data)
                    writer.write(data)
                    await writer.drain()
            except ConnectionError:
                pass
            finally:
                writer.close()
                jobs.discard(job)

        server = await asyncio.start_server(echo, '127.0.0.1', 0)
        proxy = FaultProxy(server.sockets[0].getsockname()[1])
        clients = []
        try:
            port = await proxy.start()
            reader, writer = await asyncio.open_connection('127.0.0.1', port)
            clients.append(writer)
            payload = bytes(range(256)) * 257
            writer.write(payload)
            await writer.drain()
            self.assertEqual(await asyncio.wait_for(reader.readexactly(len(payload)), 3), payload)
            await proxy.set_online(False)
            self.assertEqual(proxy.snapshot()['active'], 0)
            self.assertEqual(await asyncio.wait_for(reader.read(), 2), b'')
            denied, blocked = await asyncio.open_connection('127.0.0.1', port)
            clients.append(blocked)
            self.assertEqual(await asyncio.wait_for(denied.read(), 2), b'')
            self.assertEqual(proxy.snapshot()['upstream_opened'], 1)
            await proxy.set_online(True)
            resumed, new_writer = await asyncio.open_connection('127.0.0.1', port)
            clients.append(new_writer)
            new_writer.write(b'new-connection')
            await new_writer.drain()
            self.assertEqual(await asyncio.wait_for(resumed.readexactly(14), 2), b'new-connection')
            self.assertEqual(b''.join(messages), payload + b'new-connection')
            self.assertEqual(proxy.snapshot()['upstream_opened'], 2)
            self.assertEqual(proxy.snapshot()['rejected'], 1)
            # A lost Android route can leave the host-side accepted TCP alive.
            # Shutdown must finish while both clients and upstream remain open.
            await asyncio.wait_for(proxy.close(), 2)
            self.assertEqual(await asyncio.wait_for(resumed.read(), 2), b'')
            self.assertEqual(proxy.snapshot()['active'], 0)
        finally:
            for writer in clients:
                writer.close()
            await proxy.close()
            server.close()
            await server.wait_closed()
            pending = list(jobs)
            for job in pending:
                job.cancel()
            await asyncio.gather(*pending, return_exceptions=True)
