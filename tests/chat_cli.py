"""Run after cargo build: python3 tests/chat_cli.py (Python stdlib only).
Exercises account chat and real tool dispatch against local mock APIs, with dummy keys.
"""
from mock_stream import stream, plain

import json
import os
import pty
import select
import socket
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

BINARY = Path(__file__).resolve().parents[1] / 'target/debug/lunchmoney'


def call(name, arguments, call_id='call1'):
    return {'id': call_id, 'type': 'function',
            'function': {'name': name, 'arguments': json.dumps(arguments)}}


def api(method, path, **kwargs):
    return call('lunchmoney_api', dict(method=method, path=path, **kwargs))


def run_case(inputs, steps, read_only=False, uncertain=False, upload_allowed=False, responses=False, research_searches=0, no_web_search=False, search_failure=False, seed_cache=None, main_provider="openai", stream_probe=False, truncate=False, color=False):
    requests, ai_requests, writes, uploads, searches = [], [], [], [], []
    observed_partial = threading.Event()
    transaction = dict(id=1, payee='BANK RAW', category_id=11, amount='5.00',
                       currency='cad', original_name='COFFEE SHOP', notes='Coffee')
    temporary = tempfile.TemporaryDirectory(prefix='lunchmoney-chat-')
    attachment = Path(temporary.name) / 'receipt.txt'
    attachment.write_text('Mock receipt contents')
    inputs = [line.replace('$FILE', str(attachment)) for line in inputs]
    steps = [(reply, check) for reply, check in steps]

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def reply(self, body):
            if getattr(self, 'streaming', False) and ('choices' in body or 'output' in body):
                def after_text():
                    if stream_probe:
                        assert observed_partial.wait(3), 'Reply was buffered until stream completion'
                stream(self, body, after_text=after_text, truncate=truncate)
                return
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(json.dumps(body).encode())

        def do_GET(self):
            assert self.headers.get('Authorization') == 'Bearer dummy-lunchmoney-key'
            requests.append(('GET', self.path))
            path = self.path.split('?')[0]
            if path == '/v2/categories':
                self.reply({'categories': [{'id': 11, 'name': 'Dining'}]})
            elif path == '/v2/transactions':
                assert 'include_metadata=true' in self.path
                offset = int(self.path.split('offset=')[1].split('&')[0])
                if offset == 0:
                    self.reply({'transactions':[dict(transaction,id=91,date='2026-09-01',payee='Cafe Koo',original_name='BANK RAW',status='reviewed')], 'has_more':True})
                else:
                    assert offset == 1
                    self.reply({'transactions':[dict(transaction,id=92,date='2026-11-01',payee='Cafe Koo',original_name='BANK RAW')], 'has_more':False})
            elif path == '/v2/transactions/1':
                self.reply(transaction)
            elif path == '/v2/me':
                self.reply({'name': 'Mock User', 'primary_currency': 'cad'})
            else:
                raise AssertionError(self.path)

        def do_PUT(self):
            assert self.headers.get('Authorization') == 'Bearer dummy-lunchmoney-key'
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            requests.append(('PUT', self.path))
            writes.append((self.path, body))
            transaction.update(body)
            if uncertain:
                self.connection.shutdown(socket.SHUT_RDWR)
                self.connection.close()
            else:
                self.reply(transaction)

        def do_POST(self):
            data = self.rfile.read(int(self.headers['Content-Length']))
            if self.path == '/v2/transactions/1/attachments':
                assert self.headers.get('Authorization') == 'Bearer dummy-lunchmoney-key'
                assert self.headers['Content-Type'].startswith('multipart/form-data;')
                assert b'Mock receipt contents' in data
                uploads.append(self.path)
                self.reply({'id': 42, 'file_name': 'receipt.txt'})
                return
            body = json.loads(data)
            self.streaming = body.get('stream', False)
            if self.path == '/ai/responses' and body['tools'][0]['type'] == 'web_search':
                assert self.headers.get('Authorization') == ('Bearer dummy-research-key' if main_provider != 'openai' else 'Bearer dummy-ai-key')
                assert body['store'] is False and body['tool_choice'] == 'required'
                assert json.loads(body['input']) == {'merchant':'Cafe Koo','location':'Montreal'}
                assert b'BANK RAW' not in data and b'dummy-lunchmoney-key' not in data
                searches.append(body)
                if search_failure:
                    self.send_response(401); self.end_headers()
                    self.wfile.write(b'{"error":{"message":"dummy-ai-key"}}')
                    return
                self.reply({'id':'research','status':'completed','output':[
                    {'type':'web_search_call','status':'completed','action':{'sources':[{'url':'https://example.com/cafe','title':'Cafe Koo'}]}},
                    {'type':'message','content':[{'type':'output_text','text':'Cafe Koo is a cafe in Montreal.',
                      'annotations':[{'type':'url_citation','url':'https://example.com/cafe','title':'Cafe Koo'}]}]}]})
                return
            assert self.path == ('/ai/responses'  if responses else '/ai/chat/completions'), self.path
            assert self.headers.get('Authorization') == 'Bearer dummy-ai-key'
            assert b'dummy-lunchmoney-key' not in data and b'dummy-ai-key' not in data
            body = json.loads(data)
            assert {tool['name'] if responses else tool['function']['name'] for tool in body['tools']} == {
                'lunchmoney_api', 'lunchmoney_api_reference', 'lunchmoney_upload_attachment', 'search_transaction_history'} | (set() if no_web_search else {'research_merchant'})
            assert body['stream'] is True
            position = len(ai_requests)
            assert position < len(steps), position
            reply, check = steps[position]
            if responses:
                assert body['model'] == 'gpt-6.1-sol'
                assert body['store'] is False
                assert 'reasoning_effort' not in body
                assert body.get('reasoning', {}).get('effort') != 'none'
            if check:
                check(body)
            ai_requests.append(body)
            if isinstance(reply, list):
                reply = json.loads(json.dumps(reply).replace('$FILE', str(attachment)))
                message = {'role': 'assistant', 'content': None, 'tool_calls': reply}
            else:
                message = {'role': 'assistant', 'content': reply}
            if responses:
                if isinstance(reply, list):
                    output = [{'type':'function_call', 'id':'fc_' + tool['id'],
                               'call_id':tool['id'], 'name':tool['function']['name'],
                               'arguments':tool['function']['arguments'], 'status':'completed'}
                              for tool in reply]
                else:
                    output = [{'type':'message','id':'msg', 'role':'assistant', 'status':'completed',
                               'content':[{'type':'output_text','text':reply,'annotations':[]}]}]
                self.reply({'id':'resp_mock','status':'completed','model':'gpt-6.1-sol','output':output})
                return
            self.reply({'id': 'mock', 'object': 'chat.completion' , 'created': 0, 'model': 'mock',
                        'choices': [{'index': 0, 'message': message,
                                     'finish_reason': 'tool_calls' if isinstance(reply, list) else 'stop'}]})

    server = HTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    master, slave = pty.openpty()
    env = dict(os.environ, XDG_CONFIG_HOME=temporary.name)
    for key in ['LUNCH_MONEY_AI_MODEL', 'LUNCH_MONEY_AI_PROVIDER', 'LUNCH_MONEY_AI_API_KEY',
                'LUNCH_MONEY_AI_BASE_URL', 'OPENAI_API_KEY']:
        env.pop(key, None)
    if main_provider != 'openai':
        config_path = Path(temporary.name) / 'lunchmoney/ai.json'
        config_path.parent.mkdir()
        config_path.write_text(json.dumps({'providers':{'openai':{'model':'mock',
            'api_key':'dummy-research-key','base_url':f'http://127.0.0.1:{server.server_port}/ai'}}}))
        config_path.chmod(0o600)
    if seed_cache:
        cache_path = Path(temporary.name) / 'lunchmoney/merchant-research.json'
        cache_path.parent.mkdir()
        key = json.dumps([f'http://127.0.0.1:{server.server_port}/ai','mock','cafe koo','montreal'],separators=(',',':'))
        timestamp = int(time.time()) - (31*24*3600 if seed_cache=='expired' else 0)
        cache_path.write_text(json.dumps({'entries':{key:{'merchant':'Cafe Koo','location':'Montreal',
            'evidence':'Cafe Koo is a cafe in Montreal.','sources':[{'url':'https://example.com/cafe','title':'Cafe Koo'}],
            'researched_at':timestamp,'cached':False}}}))
        cache_path.chmod(0o600)
    command = [str(BINARY), '--token', 'dummy-lunchmoney-key' , '--base-url',
               f'http://127.0.0.1:{server.server_port}/v2', 'ai', 'chat',
               '--provider', main_provider, '--model', 'gpt-6.1-sol' if responses else 'mock', '--ai-api-key', 'dummy-ai-key',
               '--ai-base-url', f'http://127.0.0.1:{server.server_port}/ai']
    if no_web_search:
        command.append("--no-web-search")
    if read_only:
        command.append('--read-only')
    if color:
        env['TERM'] = 'xterm-256color'
        env.pop('NO_COLOR', None)
    process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, env=env)
    os.close(slave)
    output, answered, deadline = b'', 0, time.monotonic() + 20
    try:
        while time.monotonic() < deadline:
            if select.select([master], [], [], 0.1)[0]:
                try:
                    output += os.read(master, 65536)
                except OSError:
                    break
                if 'AI: Streaming ' in plain(output):
                    observed_partial.set()
                while answered < output.count(b'You: '):
                    assert answered < len(inputs), plain(output)
                    os.write(master, (inputs[answered] + '\n').encode())
                    answered += 1
            if process.poll() is not None:
                break
        assert process.wait(timeout=2) == 0, plain(output)
        assert answered == len(inputs), plain(output)
        assert len(ai_requests) == len(steps), plain(output)
        assert len(uploads) == int(upload_allowed)
        assert len(searches) == research_searches, searches
        cache_path = Path(temporary.name) / 'lunchmoney/merchant-research.json'
        if searches and not search_failure:
            cache = cache_path.read_text()
            assert 'dummy-ai-key' not in cache and 'dummy-research-key' not in cache and 'BANK RAW' not in cache
            assert 'https://example.com/cafe' in cache
            assert cache_path.stat().st_mode & 0o777 == 0o600
        elif search_failure:
            assert not cache_path.exists()
        assert 'dummy-ai-key' not in plain(output)
        assert 'dummy-research-key' not in plain(output)
        assert 'dummy-lunchmoney-key' not in plain(output)
        if stream_probe:
            assert observed_partial.is_set()
        if color:
            assert b'\x1b[2K' in output and b'\x1b[1;36m' in output
        return plain(output), requests, writes
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        os.close(master)
        server.shutdown()
        server.server_close()
        temporary.cleanup()


def results(body):
    return [json.loads(message['content']) for message in body['messages'] if message['role']=='tool']


def expect_results(*success):
    def check(body):
        assert [result['ok'] for result in results(body)][-len(success):] == list(success)
    return check


def check_history(body):
    assert any(message.get('content') == 'Renamed transaction 1.' for message in body['messages'])
    assert any(result.get('result', {}).get('payee') == 'Coffee Shop' for result in results(body))


def check_cleared(body):
    assert len(body['messages']) == 2
    assert body['messages'][-1]['content'] == 'Hello again'


def check_responses_history(body):
    outputs = [json.loads(item['output']) for item in body['input']
               if item.get('type') == 'function_call_output']
    assert outputs[-1]['ok'] is True
    assert outputs[-1]['result']['payee'] == 'BANK RAW'
    calls = [item for item in body['input'] if item.get('type') == 'function_call']
    assert calls[-1]['call_id'] == 'call1'


def check_research_result(cached):
    def check(body):
        result = results(body)[-1]
        assert result['ok'] is True
        assert result['result']['cached'] is cached
        assert result['result']['sources'][0]['url'] == 'https://example.com/cafe'
    return check


if __name__ == '__main__':
    history_tool = call('search_transaction_history',{'queries':['BANK RAW'],'before_date':'2026-10-01'})
    def check_history_matches(body):
        result = results(body)[-1]['result']
        assert result['history_scan_complete'] is True and result['matched_count'] == 1
        assert result['matches'][0]['id'] == 91
        assert result['matches'][0]['category'] == 'Dining'
    out, requests, _ = run_case(['Check prior transactions','Check them again','/quit'], [
        ([history_tool], None), ('Prior transactions were Dining.', check_history_matches),
        ([history_tool], None), ('Same session snapshot.', check_history_matches)], read_only=True, no_web_search=True)
    assert len([path for method,path in requests if path.startswith('/v2/transactions?')]) == 2
    merchant = call('research_merchant' , {'merchant':'Cafe Koo','location':'Montreal'})
    out, _, _ = run_case(['Research Cafe Koo', 'Research Cafe Koo again', '/quit'], [
        ([merchant], None), ('Cafe Koo is a cafe.', check_research_result(False)),
        ([merchant], None), ('Same cached evidence.', check_research_result(True))],
        read_only=True, research_searches=1)
    assert 'Merchant evidence: Cafe Koo · 1 source' in out and '(cached)' in out
    assert 'https://example.com/cafe' not in out
    run_case(['Research Cafe Koo', '/quit'], [([merchant], None),
        ('Using evidence from an earlier session.', check_research_result(True))], seed_cache='fresh')
    run_case(['Research Cafe Koo', '/quit'], [([merchant], None),
        ('Evidence refreshed after expiry.', check_research_result(False))], seed_cache='expired', research_searches=1)
    run_case(['Research Cafe Koo', '/quit'], [([merchant], None),
        ('Groq used OpenAI research evidence.', check_research_result(False))], main_provider='groq', research_searches=1)
    run_case(['Research Cafe Koo', '/quit'], [([merchant], None),
        ('Research unavailable.' , expect_results(False))], research_searches=1, search_failure=True)
    run_case(['Research Cafe Koo', '/quit'], [([merchant], None),
        ('Research disabled.', expect_results(False))], no_web_search=True)

    out, requests, writes = run_case(['Show transaction 1', '/quit'], [
        ([api('GET','/transactions/1')], None),
        ('The payee is BANK RAW.', check_responses_history)], responses=True)
    assert requests == [('GET','/v2/transactions/1')] and not writes
    assert 'The payee is BANK RAW.' in out and 'Chat failed' not in out

    out, requests, writes = run_case([
        'Show transaction 1 and rename it Coffee Shop', 'What is its category?',
        '/tools', '/help', '/clear', 'Hello again', '/quit'], [
        ([call('lunchmoney_api_reference', {'method':'PUT','path':'/transactions/{id}'}, 'docs'),
          call('lunchmoney_api', {'method':'GET','path':'/categories'}, 'cats')], None),
        ([api('GET','/transactions/1')], expect_results(True, True)),
        ([api('PUT','/transactions/1', body={'payee':'Coffee Shop'})], expect_results(True)),
        ('Renamed transaction 1.', expect_results(True)),
        ('Dining.', check_history), ('Hello!', check_cleared)])
    assert writes == [('/v2/transactions/1', {'payee':'Coffee Shop'})]
    assert 'PUT /me/account/settings' in out and '/crypto/synced' in out
    assert 'Conversation cleared.' in out

    mutations = [call('lunchmoney_api', {'method':method,'path':'/transactions/1'}, str(i))
                 for i, method in enumerate(['POST','PUT','DELETE'])]
    mutations.append(call('lunchmoney_upload_attachment', {'transaction_id':1,'local_path':'$FILE'}, 'upload'))
    out, requests, writes = run_case(['Make changes and attach $FILE', 'Show my profile', '/quit'], [
        (mutations, None), ('Read-only session.', expect_results(False, False, False, False)),
        ([api('GET','/me', query=[{'name':'x','value':'1'},{'name':'x','value':'2'}])], None),
        ('Mock User.', expect_results(True))], read_only=True)
    assert not writes and requests == [('GET','/v2/me?x=1&x=2')]

    out, requests, writes = run_case(['Show my profile', '/quit'], [
        ([api('GET','https://evil.example/me'), call('shell', {'cmd':'pwd'}, 'shell')], None),
        ('Invalid tools rejected.', expect_results(False, False))])
    assert not requests and not writes

    run_case(['Attach $FILE to transaction 1', '/quit'], [
        ([call('lunchmoney_upload_attachment', {'transaction_id':1,'local_path':'$FILE'})], None),
        ('Attached.', expect_results(True))], upload_allowed=True)
    run_case(['Attach a receipt to transaction 1', '/quit'], [
        ([call('lunchmoney_upload_attachment', {'transaction_id':1,'local_path':'$FILE'})], None),
        ('Please supply the receipt path.', expect_results(False))])

    out, requests, writes = run_case(['Rename transaction 1 to Coffee Shop', '/quit'], [
        ([api('PUT','/transactions/1',body={'payee':'Coffee Shop'})], None),
        ([api('PUT','/transactions/1',body={'payee':'Coffee Shop'})], expect_results(False)),
        ([api('GET','/transactions/1')], expect_results(False)),
        ('The connection failed, but the current transaction has the requested name.', expect_results(True))], uncertain=True)
    assert len(writes) == 1 and 'automatic repeat writes are blocked' in out

    piped = subprocess.run([str(BINARY), '--token', 'mock', 'ai', 'chat'], capture_output=True, text=True)
    assert piped.returncode != 0 and 'interactive terminal' in piped.stderr
    print('Account chat checks passed: web research, sourced evidence, persistent cache reuse, expiry, separate research-provider credentials, disabled search and failures, schema tools, reads, minimal writes, multi-turn context, commands, clear, read-only enforcement, URL/tool rejection, explicit-path uploads, uncertain-write retry blocking, scoped credentials, and terminal guard.')

    for responses in (False, True):
        output, _, _ = run_case(['hello', '/quit'], [('Streaming reply is here.', None)],
                               responses=responses, stream_probe=True, color=True)
        assert 'Streaming reply is here.' in output
        output, _, writes = run_case(['rename transaction 1 to Cafe', '/quit'],
                                    [([api('PUT', '/transactions/1', body={'payee':'Cafe'})], None)],
                                    responses=responses, truncate=True)
        assert not writes and 'Chat failed:' in output
    print('Streaming checks passed: text before completion, ANSI presentation, fragmented tools, and interrupted streams without writes.')

    markdown_reply = "You have **CA$1,490.70 left in Food**.\n\n- **Budget:** CA$2,500.00\n- **Spent:** CA$1,009.30\n  - Groceries: CA$655.73\n  - Restaurants / Cafes: CA$353.57\n"
    def check_raw_markdown(body):
        assert any(message['role'] == 'assistant' and message['content'] == markdown_reply
                   for message in body['messages'])
    for color in (False, True):
        output, _, _ = run_case(['Food remaining?', 'Thanks', '/quit'],
                               [(markdown_reply, None), ('You are welcome.', check_raw_markdown)], color=color)
        assert 'You have CA$1,490.70 left in Food.' in output
        assert '• Budget: CA$2,500.00' in output
        assert '  • Groceries: CA$655.73' in output
        assert '**' not in output
    print('Markdown checks passed: readable streamed formatting, nested bullets, and original Markdown preserved in conversation history.')
