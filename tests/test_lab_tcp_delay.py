"""Propagation versus serial throttling for the owned network test harness."""
import asyncio
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    'lab_tcp_delay', Path(__file__).resolve().parents[1] / 'tools/lab_tcp_delay.py')
delay = importlib.util.module_from_spec(spec)
spec.loader.exec_module(delay)


class Reader:
    def __init__(self, chunks):
        self.chunks = iter(chunks)
        self.reads = 0

    async def read(self, limit):
        self.reads += 1
        chunk = next(self.chunks, b'')
        assert len(chunk) <= limit
        return chunk


class Writer:
    def __init__(self, gate=None):
        self.data = bytearray()
        self.times = []
        self.eof = False
        self.gate = gate

    def write(self, data):
        self.data.extend(data)
        self.times.append(asyncio.get_running_loop().time())

    async def drain(self):
        if self.gate is not None:
            await self.gate.wait()

    def write_eof(self):
        self.eof = True


class DelayTests(unittest.IsolatedAsyncioTestCase):
    async def test_pipeline_overlaps_latency_without_reordering_or_byte_changes(self):
        chunks = [bytes([n]) * 16384 for n in range(8)]
        writer, counts = Writer(), [0, 0]
        start = asyncio.get_running_loop().time()
        await delay.relay(Reader(chunks), writer, 1, counts, 200, 50, 'pipeline')
        self.assertEqual(writer.data, b''.join(chunks))
        self.assertEqual(counts, [0, 8*16384])
        self.assertTrue(writer.eof)
        self.assertGreaterEqual(writer.times[0]-start, .14)
        # Eight serial sleeps require about1.6s. A generous bound leaves
        # scheduling headroom but still detects accidental per-chunk throttling.
        self.assertLess(writer.times[-1]-start, 1.2)

    async def test_serial_preserves_prior_adversarial_shaper(self):
        writer, counts = Writer(), [0, 0]
        start = asyncio.get_running_loop().time()
        await delay.relay(Reader([b'a', b'b', b'c']), writer, 0, counts, 60, 0, 'serial')
        self.assertEqual(writer.data, b'abc')
        self.assertEqual(counts, [3, 0])
        self.assertGreaterEqual(writer.times[-1]-start, .17)
        self.assertTrue(writer.eof)

    async def test_blocked_output_bounds_reads_and_cancellation_stops_both_tasks(self):
        reader = Reader([b'x'*16384]*1000)
        writer, counts = Writer(asyncio.Event()), [0, 0]
        task = asyncio.create_task(delay.relay(reader, writer, 0, counts, 0, 0, 'pipeline'))
        await asyncio.sleep(.03)
        self.assertLessEqual(reader.reads, 10)
        self.assertEqual(len(writer.data), 16384)
        self.assertEqual(counts, [0, 0])
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        before = reader.reads
        await asyncio.sleep(.03)
        self.assertEqual(reader.reads, before)
        self.assertFalse(writer.eof)

    async def test_output_failure_cancels_reader_without_claiming_eof(self):
        class FailedWriter(Writer):
            async def drain(self):
                raise ConnectionError('owned fixture closed')
        reader, writer = Reader([b'x']*1000), FailedWriter()
        with self.assertRaises(ExceptionGroup):
            await delay.relay(reader, writer, 0, [0, 0], 0, 0, 'pipeline')
        self.assertLessEqual(reader.reads, 10)
        self.assertFalse(writer.eof)

    async def test_empty_stream_half_closes_without_fake_data(self):
        for mode in ('serial', 'pipeline'):
            writer, counts = Writer(), [0, 0]
            await delay.relay(Reader([]), writer, 0, counts, 200, 50, mode)
            self.assertEqual(writer.data, b'')
            self.assertEqual(counts, [0, 0])
            self.assertTrue(writer.eof)

    async def test_actual_loopback_delivers_delayed_bytes_then_half_close(self):
        received = asyncio.get_running_loop().create_future()

        async def accepted(reader, writer):
            counts = [0, 0]
            try:
                await delay.relay(reader, writer, 0, counts, 40, 10, 'pipeline')
                received.set_result(counts)
            except BaseException as error:
                received.set_exception(error)
            finally:
                writer.close()
                await writer.wait_closed()

        server = await asyncio.start_server(accepted, '127.0.0.1', 0, limit=65536)
        async with server:
            reader, writer = await asyncio.open_connection('127.0.0.1', server.sockets[0].getsockname()[1])
            try:
                payload = bytes(range(256))*2048
                writer.write(payload)
                await writer.drain()
                writer.write_eof()
                self.assertEqual(await asyncio.wait_for(reader.readexactly(len(payload)), 3), payload)
                self.assertEqual(await asyncio.wait_for(reader.read(1), 3), b'')
                self.assertEqual(await received, [len(payload), 0])
            finally:
                writer.close()
                await writer.wait_closed()


if __name__ == '__main__':
    unittest.main()
