"""Run after cargo build: python3 tests/review_cli.py (Python stdlib; openssl for HTTPS fixture).
Exercises the real binary through a pseudo-terminal and mock Lunch Money / AI APIs.
"""
import copy
from mock_stream import stream, plain

import json
import os
import pty
import select
import ssl
import subprocess
import threading
import tempfile
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

BINARY = Path(__file__).resolve().parents[1] / 'target/debug/lunchmoney'
TRANSACTIONS = [dict(id=i, date='2026-10-01', payee='BANK RAW', original_name='BANK RAW ORIGINAL',
                     amount='32.98', currency='cad', category_id=30, notes='Dinner', status='unreviewed',
                     updated_at='v1', plaid_metadata={'merchant_name': 'Cafe','transaction':{'category':['Food and Drink','Restaurants'],'category_id':'13005000'}},
                     custom_metadata={'purpose': 'meal'}, children=[{'id': 100 + i}],
                     files=[{'id': 200 + i}]) for i in (1, 2)]


def run_case(actions, expected_body=None, invalid=False, stale=False, saved=False, jev=False, tls_failure=False, no_changes=False, chat=False, chat_failure=False, chat_override=False, research=False, targeted=False, uncertain=False, chat_revision=False, revision_invalid=False):
    writes, prompts, requests = [], [], []
    detail_reads = {}
    conversations, searches, review_rounds = [], [], []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def reply(self, value):
            if getattr(self, 'streaming', False) and 'choices' in value:
                stream(self, value)
                return
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(json.dumps(value).encode())

        def do_GET(self):
            parsed = urllib.parse.urlparse(self.path)
            path = parsed.path
            requests.append((path, urllib.parse.parse_qs(parsed.query)))
            if path == '/v2/transactions':
                query = urllib.parse.parse_qs(parsed.query)
                if query.get('limit') == ['1000']:
                    assert query['include_metadata'] == ['true']
                    history = [dict(TRANSACTIONS[0], id=91, date='2026-09-01', payee='Cafe', category_id=11, status='reviewed'),
                               dict(TRANSACTIONS[0], id=92, date='2026-08-01', category_id=30, status='unreviewed'),
                               dict(TRANSACTIONS[0], id=93, date='2026-11-01', category_id=11),
                               *TRANSACTIONS]
                    self.reply({'transactions':history, 'has_more':False})
                else:
                    self.reply({'transactions': TRANSACTIONS, 'has_more': False})
            elif path.startswith('/v2/transactions/'):
                tx_id = int(path.rsplit('/', 1)[1])
                transaction = copy.deepcopy(TRANSACTIONS[tx_id - 1])
                detail_reads[tx_id] = detail_reads.get(tx_id, 0) + 1
                if stale and detail_reads[tx_id] > 1:
                    transaction['payee'] = 'Changed elsewhere'
                self.reply(transaction)
            elif path == '/v2/categories':
                self.reply({'categories': [
                    {'id': 10, 'name': 'Food', 'is_group': True, 'children': [
                        {'id': 11, 'name': 'Dining', 'description': 'Restaurant meals'}]},
                    {'id': 30, 'name': 'Transfers'}]})
            elif path == '/v2/me':
                self.reply({'primary_currency': 'cad'})
            elif path == '/v2/plaid_accounts':
                self.reply({'plaid_accounts': []})
            elif path == '/v2/manual_accounts':
                self.reply({'manual_accounts': []})
            elif path == '/v2/tags':
                self.reply({'tags': []})
            else:
                raise AssertionError(path)

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            self.streaming = body.get('stream', False)
            if self.path == '/ai/responses':
                assert research and body['tools'] == [{'type':'web_search'}]
                assert self.headers.get('Authorization') == 'Bearer mock-ai'
                assert body['store'] is False and body['tool_choice'] == 'required'
                assert json.loads(body['input']) == {'merchant':'Cafe','location':'Montreal'}
                assert 'transactions' not in body['input'] and 'Dinner' not in body['input']
                searches.append(body)
                self.reply({'status':'completed','output':[
                    {'type':'web_search_call','status':'completed'},
                    {'type':'message','content':[{'type':'output_text','text':'Cafe is a restaurant in Montreal.',
                     'annotations':[{'type':'url_citation','url':'https://example.com/cafe','title':'Cafe'}]}]}]})
                return
            if jev and self.path == '/ai/systemone':
                assert self.path == '/ai/systemone', self.path
                assert body['model'] == 'jev-latest'
                assert 'category_11' in body['questions']['tx_1_category']['criteria']
                prompt = body['state']
                prompts.append(prompt)
                answers = {}
                for tx in prompt['transactions']:
                    prefix = f"tx_{tx['id']}_"
                    names = body['questions'][prefix+'name']['criteria']
                    name_key = next(key for key, name in names.items() if name == 'Cafe')
                    answers[prefix+'category'] = {'type': 'choice', 'choice': 'uncertain' if no_changes else 'category_999' if invalid else 'category_11', 'confidence': 0.9}
                    answers[prefix+'name'] = {'type': 'choice', 'choice': 'keep' if no_changes else name_key, 'confidence': 0.95}
                self.reply({'answers': answers})
                return
            assert self.path == '/ai/chat/completions', self.path
            if 'response_format' not in body:
                assert self.headers.get('Authorization') == ('Bearer mock-chat-key' if chat_override else 'Bearer mock-ai')
                conversations.append(body['messages'])
                user_messages = [msg for msg in body['messages'] if msg['role'] == 'user']
                context = json.loads(user_messages[0]['content'])
                assert context['transactions'] == [TRANSACTIONS[0]]
                assert context['suggestion']['category_id'] == (None if uncertain or targeted and no_changes else 11)
                assert context['assignable_categories'][0]['path'] == 'Food / Dining'
                if chat_failure:
                    self.send_response(500)
                    self.send_header('Content-Type', 'application/json')
                    self.end_headers()
                    self.wfile.write(b'{"error":{"message":"Mock chat failed"}}')
                    return
                if chat_revision:
                    assert not writes, 'Chat must never save a pending revision'
                    assert 'revise_review_suggestion' in {tool['function']['name'] for tool in body['tools']}
                    results = [json.loads(msg['content']) for msg in body['messages'] if msg['role']=='tool']
                    if not results:
                        arguments = {'reason':'User requested a placeholder title','payee':'(unknown)'}
                        if revision_invalid: arguments['category_id'] = 999
                        self.reply({'id':'mock-chat','model':'mock','choices':[{'index':0,'message':{
                            'role':'assistant','content':None,'tool_calls':[{'id':'revise','type':'function','function':{
                                'name':'revise_review_suggestion','arguments':json.dumps(arguments)}}]},'finish_reason':'tool_calls'}]})
                    else:
                        assert results[-1]['ok'] is not revision_invalid
                        if not revision_invalid:
                            assert results[-1]['result']['saved'] is False
                            pending = results[-1]['result']['pending_suggestion']
                            assert pending['payee'] == '(unknown)' and pending['category_id'] is None
                        self.reply({'id':'mock-chat','model':'mock','choices':[{'index':0,'message':{
                            'role':'assistant','content':'Pending revision rejected.' if revision_invalid else 'Pending title changed to (unknown); not saved.'},'finish_reason':'stop'}]})
                    return
                if len(conversations) > 1:
                    assert any(msg['role']=='assistant' and msg['content']=='The notes indicate a restaurant meal.' for msg in body['messages'])
                self.reply({'id': 'mock-chat', 'object': 'chat.completion', 'created': 0, 'model': 'mock',
                    'choices': [{'index': 0, 'message': {'role': 'assistant', 'content': 'The notes indicate a restaurant meal.'}, 'finish_reason': 'stop'}]})
                return
            prompt = json.loads(next(msg['content'] for msg in body['messages'] if msg['role']=='user'))
            if research:
                review_rounds.append(body)
                assert {tool['function']['name'] for tool in body['tools']} == {'research_merchant','search_transaction_history'}
                if len(review_rounds) == 1:
                    prompts.append(prompt)
                    tools = [{'id':f'research_{i}','type':'function','function':{'name':'research_merchant',
                             'arguments':json.dumps({'merchant':'Cafe','location':'Montreal'})}} for i in (1,2)]
                    self.reply({'id':'mock','model':'mock','choices':[{'index':0,'message':{'role':'assistant','content':None,'tool_calls':tools},'finish_reason':'tool_calls'}]})
                    return
                results = [json.loads(msg['content']) for msg in body['messages'] if msg['role']=='tool']
                assert len(results) == 2 and all(result['ok'] for result in results)
                assert results[0]['result']['cached'] is False
                assert results[1]['result']['cached'] is True
                assert prompt['transactions'] == TRANSACTIONS
            else:
                prompts.append(prompt)
            suggestion = {'suggestions': [] if no_changes else [dict(transaction_id=tx['id'],
                category_id=999 if invalid else 11, payee='Cafe',
                reason='Merchant and notes indicate a restaurant meal.', confidence='high') for tx in prompt['transactions']]}
            if uncertain:
                suggestion = {'suggestions':[dict(transaction_id=tx['id'],category_id=None,payee=None,
                    needs_review=True,reason='Merchant identity is unresolved; the bank abbreviation is ambiguous.',confidence='low') for tx in prompt['transactions']]}
            self.reply({'id': 'mock', 'object': 'chat.completion', 'created': 0, 'model': 'mock',
                        'choices': [{'index': 0, 'message': {'role': 'assistant', 'content': json.dumps(suggestion)},
                                     'finish_reason': 'stop'}],
                        'usage': {'prompt_tokens': 100, 'completion_tokens': 30, 'total_tokens': 130}})

        def do_PUT(self):
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            writes.append((self.path, body))
            self.reply(dict(TRANSACTIONS[0], **body))

    server = HTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    master, slave = pty.openpty()
    temporary = tempfile.TemporaryDirectory(prefix='lunchmoney-ai-cli-')
    tls_server = None
    ai_url = f'http://127.0.0.1:{server.server_port}/ai'
    if tls_failure:
        # Self-signed HTTPS must reach certificate validation. With TLS disabled this
        # would fail earlier as an unsupported scheme, hiding behind the SDK wrapper.
        cert, key = Path(temporary.name) / 'cert.pem', Path(temporary.name) / 'key.pem'
        subprocess.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes',
                        '-keyout', str(key), '-out', str(cert), '-days', '1',
                        '-subj', '/CN=localhost'], check=True, capture_output=True)
        tls_server = HTTPServer(('127.0.0.1', 0), Handler)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(cert, key)
        tls_server.socket = context.wrap_socket(tls_server.socket, server_side=True)
        threading.Thread(target=tls_server.serve_forever, daemon=True).start()
        ai_url = f'https://localhost:{tls_server.server_port}/ai'
    child_env = dict(os.environ, COLUMNS='160', XDG_CONFIG_HOME=temporary.name)
    for key in ['LUNCH_MONEY_AI_MODEL', 'LUNCH_MONEY_AI_PROVIDER', 'LUNCH_MONEY_AI_API_KEY', 'LUNCH_MONEY_AI_BASE_URL', 'OPENAI_API_KEY', 'TYPESAFE_API_KEY']:
        child_env.pop(key, None)
    ai_options = ['--model', 'openai::mock', '--ai-api-key', 'mock-ai', '--ai-base-url', ai_url]
    if saved:
        def config_command(*args):
            return subprocess.run([str(BINARY), *args], capture_output=True, text=True, env=child_env, check=True)
        provider = 'jev' if jev else 'openai'
        model = 'jev-latest' if jev else 'mock'
        config_command('ai', 'set', '--provider', provider, '--model', model, '--ai-base-url', f'http://127.0.0.1:{server.server_port}/ai')
        config_command('ai', 'login', '--api-key', 'mock-ai')
        status = config_command('ai', 'status').stdout
        assert 'mock-ai' not in status
        assert json.loads(status)['providers'][provider]['token_saved'] is True
        config_command('ai', 'set', '--provider', 'anthropic', '--model', 'claude-test')
        config_command('ai', 'set', '--provider', provider)
        status = json.loads(config_command('ai', 'status').stdout)
        assert status['providers'][provider]['model'] == model
        assert status['providers']['anthropic']['token_saved'] is False
        ai_options = []
        if chat_override:
            config_command('ai', 'set', '--provider', 'openai', '--model', 'mock', '--ai-base-url', ai_url)
            config_command('ai', 'login', '--api-key', 'mock-chat-key')
            config_command('ai', 'set', '--provider', provider)
            ai_options = ['--chat-provider', 'openai']
    process = subprocess.Popen([
        str(BINARY), '--token', 'mock-lunchmoney', '--base-url', f'http://127.0.0.1:{server.server_port}/v2',
        'transactions', 'review', '--month', '2026-10', *(['--payee','BANK'] if targeted else []), *ai_options,
    ], stdin=slave, stdout=slave, stderr=slave, env=child_env)
    os.close(slave)
    output, answered, deadline = b'', 0, time.monotonic() + 20
    try:
        while time.monotonic() < deadline:
            if select.select([master], [], [], 0.1)[0]:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    break
                output += chunk
                # Replies are sent only when the real CLI asks for a decision.
                count = output.count(b'[q] quit: ') + output.count(b'You (/back): ')
                while answered < count:
                    assert answered < len(actions), plain(output)
                    os.write(master, (actions[answered] + '\n').encode())
                    answered += 1
            if process.poll() is not None:
                break
        assert (process.wait(timeout=2) != 0) == (invalid or tls_failure), plain(output)
        assert answered == len(actions), plain(output)
        assert writes == ([] if expected_body is None else [('/v2/transactions/1', expected_body)]), (writes, plain(output))
        if (uncertain or targeted and no_changes) and not (chat_revision and not revision_invalid):
            assert '[a] accept' not in plain(output)
            assert 'Needs clarification' in plain(output)
            assert '[t] chat about this' in plain(output)
            assert not writes
        if targeted:
            assert prompts[0]['user_requested_payee'] == 'BANK'
        if research:
            assert len(searches) == 1 and len(review_rounds) == 2
            assert 'Merchant evidence: Cafe · 1 source' in plain(output)
            assert 'https://example.com/cafe' not in plain(output)
            assert '(cached)' in plain(output)
        if tls_failure:
            assert not prompts, 'Untrusted HTTPS must never receive transaction data'
            assert 'certificate' in plain(output).lower(), plain(output)
            assert 'AI connection failed:' in plain(output)
        else:
            assert len(prompts) == 1 and prompts[0]['transactions'] == TRANSACTIONS
            assert prompts[0]['assignable_categories'][0]['path'] == 'Food / Dining'
            assert prompts[0]['bank_category_evidence'][0]['hints'][0]['legacy_category'] == ['Food and Drink','Restaurants']
            assert prompts[0]['bank_category_evidence'][0]['hints'][0]['plaid_category_id'] == '13005000'
            history = prompts[0]['related_transaction_history'][0]['history']
            assert history['matched_count'] == 2
            assert [tx['id'] for tx in history['matches']] == [91,92]
            assert history['matches'][0]['category'] == 'Food / Dining'
            assert history['matches'][0]['status'] == 'reviewed'
            assert history['history_scan_complete'] is True
            assert sum(path=='/v2/transactions' and query.get('limit')==['1000'] for path,query in requests) == 1
        if invalid or tls_failure:
            assert '[a] accept' not in plain(output)
            assert '[c] category only' not in plain(output)
        assert requests[0][1]['start_date'] == ['2026-10-01']
        assert requests[0][1]['end_date'] == ['2026-10-31']
        assert 'mock-ai' not in plain(output)
        assert 'mock-chat-key' not in plain(output)
        if chat:
            assert len(conversations) == 2
            assert 'AI: The notes indicate a restaurant meal.' in plain(output)
        if chat_failure:
            assert len(conversations) == 1
            assert 'Chat failed:' in plain(output)
        if chat_revision:
            assert len(conversations) == 2
            if not revision_invalid: assert "Pending suggestion revised; not saved." in output.decode(errors="replace")
        if not chat and not chat_failure and not chat_revision:
            assert not conversations
        if saved:
            config_command('ai', 'logout')
            status = json.loads(config_command('ai', 'status').stdout)
            assert status['providers'][provider]['token_saved'] is False
            assert status['providers'][provider]['model'] == model
        return plain(output)
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        os.close(master)
        server.shutdown()
        server.server_close()
        if tls_server is not None:
            tls_server.shutdown()
            tls_server.server_close()
        temporary.cleanup()


if __name__ == '__main__':
    run_case(['t','Set the title to (unknown)', '/back','n','q'], {'payee':'(unknown)'}, no_changes=True, targeted=True, chat_revision=True)
    run_case(['t','Set the title to (unknown)', '/back','s','q'], no_changes=True, targeted=True, chat_revision=True)
    run_case(['t','Change the suggestion', '/back','s','q'], no_changes=True, targeted=True, chat_revision=True, revision_invalid=True)
    run_case(['s','q'], uncertain=True)
    run_case(['t','Who is the merchant?', 'What evidence is missing?', '/back', 's','q'], no_changes=True, targeted=True, chat=True)
    run_case(['a', 'q'], {'category_id':11, 'payee':'Cafe'}, research=True)
    run_case(['a', 's'], {'category_id': 11, 'payee': 'Cafe'})
    run_case(['t', 'Why this category?', 'What about the name?', '/back', 'a', 'q'],
             {'category_id': 11, 'payee': 'Cafe'}, chat=True)
    run_case(['t', 'Why this category?', 's', 'q'], chat_failure=True)
    run_case(['t', 'Why this category?', 'What about the name?', '/back', 's', 'q'],
             saved=True, jev=True, chat=True, chat_override=True)
    assert 'Jev cannot generate chat replies' in run_case(['t', 'q'], saved=True, jev=True)
    run_case(['c', 'q'], {'category_id': 11})
    run_case(['n', 'q'], {'payee': 'Cafe'})
    run_case(['d', '', 'q'])
    run_case([], invalid=True)
    run_case([], tls_failure=True)
    run_case([], no_changes=True)
    run_case([], no_changes=True, saved=True, jev=True)
    run_case(['a', 'q'], stale=True)
    run_case(['a', 'q'], {'category_id': 11, 'payee': 'Cafe'}, saved=True)
    run_case(['a', 'q'], {'category_id': 11, 'payee': 'Cafe'}, saved=True, jev=True)
    run_case([], invalid=True, saved=True, jev=True)
    piped = subprocess.run([str(BINARY), '--token', 'mock', 'transactions', 'review', '--model', 'ollama::mock'],
                           capture_output=True, text=True)
    assert piped.returncode != 0 and 'interactive terminal' in piped.stderr
    print('Review CLI checks passed: batched web research and cache reuse, contextual multi-turn chat, chat errors return to review, Jev chat provider fallback, one batch for all selected transactions, no-change batches, accept both/category/name, skip, details, quit, invalid AI response, HTTPS certificate validation and error details, concurrent edit, full context, month scope, saved provider settings, scoped tokens, Jev, logout, and terminal guard.')
