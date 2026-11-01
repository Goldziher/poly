import json,os,sys; from pathlib import Path; Path(os.environ['POLY_CMD_CAPTURE']).write_text(json.dumps({'argv':sys.argv[1:],'cwd':os.getcwd()}),encoding='utf-8')
