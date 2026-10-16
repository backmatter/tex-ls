"""Regression checks for benchmark RPC deadlines and cancellation retries."""
import importlib.util
from pathlib import Path
import queue
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('rpc_benchmark', ROOT / 'scripts/benchmark_lsp_rpc.py')
RPC = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RPC)

class Clock:
    now = 0.0
    def monotonic(self):
        return self.now

class Messages:
    def __init__(self, clock, events):
        self.clock, self.events, self.timeouts = clock, iter(events), []
    def get(self, timeout):
        self.timeouts.append(timeout)
        elapsed, message = next(self.events)
        if elapsed > timeout:
            self.clock.now += timeout
            raise queue.Empty
        self.clock.now += elapsed
        return message

class RequestContract(unittest.TestCase):
    def client(self, events):
        clock = Clock()
        client = object.__new__(RPC.Client)
        client.next_id = 0
        client.messages = Messages(clock, events)
        sent = []
        client.send = lambda *args, **kwargs: sent.append((args, kwargs))
        return client, clock, sent

    def test_notifications_and_retry_share_one_deadline(self):
        client, clock, sent = self.client([
            (12, {'method': 'window/logMessage', 'params': {}}),
            (12, {'id': 1, 'error': {'code': -32801}}),
            (1, {'id': 2, 'result': 'done'}),
        ])
        with patch.object(RPC.time, 'monotonic', clock.monotonic):
            self.assertEqual(client.request('textDocument/definition', {}), 'done')
        self.assertEqual(len(sent), 2)
        self.assertEqual(client.messages.timeouts, [30, 18, 6])

    def test_notifications_cannot_extend_deadline(self):
        client, clock, _ = self.client([
            (20, {'method': 'window/logMessage', 'params': {}}),
            (11, {'id': 1, 'result': 'late'}),
        ])
        with patch.object(RPC.time, 'monotonic', clock.monotonic):
            with self.assertRaises((TimeoutError, queue.Empty)):
                client.request('textDocument/definition', {})
        self.assertLessEqual(clock.now, 30)

    def test_ready_status_calls_share_the_settle_deadline(self):
        busy = {'pendingReads': 1, 'pendingFileAcquisitions': 0,
                'pendingCompilerAcquisitions': 0, 'installationLoading': False}
        idle = {**busy, 'pendingReads': 0}
        client, clock, sent = self.client([
            (29, {'id': 1, 'result': busy}),
            (2, {'id': 2, 'result': idle}),
        ])
        def sleep(seconds):
            clock.now += seconds
        with patch.object(RPC.time, 'monotonic', clock.monotonic), patch.object(RPC.time, 'sleep', sleep):
            with self.assertRaises((TimeoutError, queue.Empty)):
                client.ready()
        self.assertEqual(len(sent), 2)
        self.assertLessEqual(clock.now, 30)
        self.assertAlmostEqual(client.messages.timeouts[0], 30)
        self.assertAlmostEqual(client.messages.timeouts[1], .99)

    def test_ready_status_notifications_cannot_extend_the_settle_deadline(self):
        idle = {'pendingReads': 0, 'pendingFileAcquisitions': 0,
                'pendingCompilerAcquisitions': 0, 'installationLoading': False}
        client, clock, _ = self.client([
            (20, {'method': 'window/logMessage', 'params': {}}),
            (11, {'id': 1, 'result': idle}),
        ])
        with patch.object(RPC.time, 'monotonic', clock.monotonic):
            with self.assertRaises((TimeoutError, queue.Empty)):
                client.ready()
        self.assertLessEqual(clock.now, 30)

    def test_only_explicit_true_retriggers_diagnostics(self):
        for data in [None, {}, {'retriggerRequest': None}, {'retriggerRequest': False},
                     {'retriggerRequest': 1}, {'retriggerRequest': 'true'}]:
            with self.subTest(data=data):
                error = {'code': -32802, 'data': data}
                client, clock, sent = self.client([(0, {'id': 1, 'error': error}),
                                                  (0, {'id': 2, 'result': 'hidden error'})])
                with patch.object(RPC.time, 'monotonic', clock.monotonic):
                    with self.assertRaises(RuntimeError):
                        client.request('textDocument/diagnostic', {})
                self.assertEqual(len(sent), 1)

    def test_legal_retries_and_other_errors(self):
        for method, error, legal in [
            ('textDocument/diagnostic', {'code': -32802, 'data': {'retriggerRequest': True}}, True),
            ('workspace/diagnostic', {'code': -32802, 'data': {'retriggerRequest': True}}, True),
            ('textDocument/definition', {'code': -32801}, True),
            ('textDocument/definition', {'code': -32802, 'data': {'retriggerRequest': True}}, False),
            ('textDocument/diagnostic', {'code': -32603}, False),
        ]:
            with self.subTest(method=method, error=error):
                client, clock, sent = self.client([(0, {'id': 1, 'error': error}),
                                                  (0, {'id': 2, 'result': 'done'})])
                with patch.object(RPC.time, 'monotonic', clock.monotonic):
                    if legal:
                        self.assertEqual(client.request(method, {}), 'done')
                    else:
                        with self.assertRaises(RuntimeError):
                            client.request(method, {})
                self.assertEqual(len(sent), 2 if legal else 1)

if __name__ == '__main__':
    unittest.main()
