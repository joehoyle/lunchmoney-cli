"""Provider-shaped SSE fixtures, including fragmented function arguments."""
import json
import re


def plain(output):
    return re.sub(r'\x1b\[[0-9;]*[A-Za-z]', '', output.decode(errors='replace'))


def stream(handler, body, after_text=None, truncate=False):
    handler.send_response(200)
    handler.send_header('Content-Type', 'text/event-stream')
    handler.end_headers()

    def emit(event):
        handler.wfile.write(('data: ' + json.dumps(event) + '\n\n').encode())
        handler.wfile.flush()

    if 'output' in body:
        emit({'type': 'response.created', 'response': dict(body, status='in_progress')})
        for index, item in enumerate(body['output']):
            if item['type'] == 'function_call':
                emit({'type':'response.output_item.added', 'output_index':index,
                      'item':dict(item, arguments='')})
                args = item['arguments']
                for chunk in (args[:len(args)//2], args[len(args)//2:]):
                    emit({'type':'response.function_call_arguments.delta',
                          'output_index':index, 'delta':chunk})
            elif item['type'] == 'message':
                for part in item['content']:
                    text = part['text']
                    for n, chunk in enumerate((text[:len(text)//2], text[len(text)//2:])):
                        emit({'type':'response.output_text.delta', 'delta':chunk})
                        if n == 0 and after_text: after_text()
        if not truncate:
            emit({'type':'response.completed', 'response':body})
        return

    choice = body['choices'][0]
    message = choice['message']

    def delta(value, finish=None):
        emit({'id':'mock', 'model':'mock', 'choices':[{'index':0, 'delta':value, 'finish_reason':finish}]})

    delta({'role':'assistant'})
    if message.get('content'):
        text = message['content']
        for n, chunk in enumerate((text[:len(text)//2], text[len(text)//2:])):
            delta({'content':chunk})
            if n == 0 and after_text: after_text()
    for index, tool in enumerate(message.get('tool_calls', [])):
        args = tool['function']['arguments']
        delta({'tool_calls':[dict(tool, index=index, function=dict(tool['function'], arguments=args[:len(args)//2]))]})
        delta({'tool_calls':[{'index':index, 'function':{'arguments':args[len(args)//2:]}}]})
    if not truncate:
        delta({}, choice['finish_reason'])
        handler.wfile.write(b'data: [DONE]\n\n')
        handler.wfile.flush()
