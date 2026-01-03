import WebSocket from 'ws';
import { EventEmitter } from 'events';

export interface CommandOutput {
  type: 'Stdout' | 'Stderr' | 'Exit' | 'Error';
  line?: string;
  code?: number;
  message?: string;
}

export interface FileContent {
  content: string;
}

export interface VcsStatus {
  vcs_type: 'Git' | 'Jj' | 'None';
  branch?: string;
  modified: string[];
  added: string[];
  deleted: string[];
}

export class ShipClient extends EventEmitter {
  private ws: WebSocket | null = null;
  private authenticated = false;
  private requestId = 0;
  private pendingRequests = new Map<number, {
    resolve: (value: any) => void;
    reject: (error: Error) => void;
  }>();

  constructor(
    private shipUrl: string,
    private sessionToken?: string,
    private ancillaryId?: string,
    private segment?: string
  ) {
    super();
  }

  async connect(): Promise<void> {
    return new Promise((resolve, reject) => {
      const wsUrl = this.shipUrl.replace(/^http/, 'ws') + '/ws';
      this.ws = new WebSocket(wsUrl);

      this.ws.on('open', async () => {
        const ancillaryName = this.ancillaryId || 'Unknown Ancillary';
        console.log(`${ancillaryName} connected to Toren`);

        if (this.sessionToken) {
          await this.authenticate(this.sessionToken);
        }

        resolve();
      });

      this.ws.on('message', (data: Buffer) => {
        try {
          const message = JSON.parse(data.toString());
          this.handleMessage(message);
        } catch (error) {
          console.error('Failed to parse message:', error);
        }
      });

      this.ws.on('error', (error) => {
        console.error('WebSocket error:', error);
        reject(error);
      });

      this.ws.on('close', () => {
        const ancillaryName = this.ancillaryId || 'Ancillary';
        console.log(`${ancillaryName} disconnected from Toren`);
        this.authenticated = false;
        this.emit('disconnect');
      });
    });
  }

  async disconnect(): Promise<void> {
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
  }

  private async authenticate(token: string): Promise<void> {
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        reject(new Error('Authentication timeout'));
      }, 5000);

      const handler = (message: any) => {
        if (message.type === 'AuthSuccess') {
          clearTimeout(timeout);
          this.authenticated = true;
          console.log('Authenticated with daemon');
          resolve();
        } else if (message.type === 'AuthFailure') {
          clearTimeout(timeout);
          reject(new Error(`Authentication failed: ${message.reason}`));
        }
      };

      this.once('message', handler);
      this.send({
        type: 'Auth',
        token,
        ancillary_id: this.ancillaryId,
        segment: this.segment
      });
    });
  }

  private handleMessage(message: any): void {
    this.emit('message', message);

    // Handle request-response pattern
    if (message.requestId !== undefined) {
      const pending = this.pendingRequests.get(message.requestId);
      if (pending) {
        if (message.type === 'Error') {
          pending.reject(new Error(message.message));
        } else {
          pending.resolve(message);
        }
        this.pendingRequests.delete(message.requestId);
      }
    }

    // Emit specific events
    switch (message.type) {
      case 'CommandOutput':
        this.emit('commandOutput', message.output);
        break;
      case 'FileContent':
        this.emit('fileContent', message.content);
        break;
      case 'VcsStatus':
        this.emit('vcsStatus', message.status);
        break;
    }
  }

  private send(message: any): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      throw new Error('WebSocket not connected');
    }

    this.ws.send(JSON.stringify(message));
  }

  async readFile(path: string): Promise<string> {
    return new Promise((resolve, reject) => {
      const requestId = this.requestId++;

      this.pendingRequests.set(requestId, { resolve, reject });

      this.send({
        type: 'FileRead',
        path,
        requestId
      });

      // Timeout after 10 seconds
      setTimeout(() => {
        if (this.pendingRequests.has(requestId)) {
          this.pendingRequests.delete(requestId);
          reject(new Error('Request timeout'));
        }
      }, 10000);
    });
  }

  async executeCommand(
    command: string,
    args: string[],
    cwd?: string
  ): Promise<AsyncIterable<CommandOutput>> {
    const outputs: CommandOutput[] = [];
    let isDone = false;

    const handler = (output: CommandOutput) => {
      outputs.push(output);
      if (output.type === 'Exit' || output.type === 'Error') {
        isDone = true;
      }
    };

    this.on('commandOutput', handler);

    this.send({
      type: 'Command',
      request: {
        command,
        args,
        cwd
      }
    });

    // Return async iterable
    return {
      async *[Symbol.asyncIterator]() {
        let index = 0;
        while (!isDone || index < outputs.length) {
          if (index < outputs.length) {
            yield outputs[index++];
          } else {
            // Wait a bit for more outputs
            await new Promise(resolve => setTimeout(resolve, 100));
          }
        }
      }
    };
  }

  async getVcsStatus(path: string): Promise<VcsStatus> {
    return new Promise((resolve, reject) => {
      const requestId = this.requestId++;

      this.pendingRequests.set(requestId, { resolve, reject });

      this.send({
        type: 'VcsStatus',
        path,
        requestId
      });

      setTimeout(() => {
        if (this.pendingRequests.has(requestId)) {
          this.pendingRequests.delete(requestId);
          reject(new Error('Request timeout'));
        }
      }, 10000);
    });
  }

  isAuthenticated(): boolean {
    return this.authenticated;
  }
}
