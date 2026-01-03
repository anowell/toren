import Anthropic from '@anthropic-ai/sdk';
import { ShipClient } from './ship-client.js';

export class AncillaryRuntime {
  private anthropic: Anthropic;
  private running = false;

  constructor(
    apiKey: string,
    private shipClient: ShipClient,
    private ancillaryId: string
  ) {
    this.anthropic = new Anthropic({ apiKey });
  }

  async start(): Promise<void> {
    this.running = true;
    console.log(`${this.ancillaryId} runtime active`);

    // Wait for authentication
    if (!this.shipClient.isAuthenticated()) {
      console.log('Waiting for ship authentication...');
      await new Promise<void>((resolve) => {
        const check = setInterval(() => {
          if (this.shipClient.isAuthenticated()) {
            clearInterval(check);
            resolve();
          }
        }, 100);
      });
    }

    console.log(`${this.ancillaryId} ready for directives`);
  }

  async stop(): Promise<void> {
    this.running = false;
    console.log(`${this.ancillaryId} runtime stopped`);
  }

  async processDirective(
    directive: string,
    context?: any,
    workingDirectory?: string
  ): Promise<string> {
    console.log(`\n🎯 Processing directive: ${directive.substring(0, 100)}...`);

    const messages: Anthropic.Messages.MessageParam[] = [
      {
        role: 'user',
        content: directive
      }
    ];

    const tools: Anthropic.Messages.Tool[] = [
      {
        name: 'read_file',
        description: 'Read the contents of a file from the filesystem',
        input_schema: {
          type: 'object',
          properties: {
            path: {
              type: 'string',
              description: 'The path to the file to read'
            }
          },
          required: ['path']
        }
      },
      {
        name: 'write_file',
        description: 'Write content to a file on the filesystem',
        input_schema: {
          type: 'object',
          properties: {
            path: {
              type: 'string',
              description: 'The path to the file to write'
            },
            content: {
              type: 'string',
              description: 'The content to write to the file'
            }
          },
          required: ['path', 'content']
        }
      },
      {
        name: 'list_directory',
        description: 'List the contents of a directory',
        input_schema: {
          type: 'object',
          properties: {
            path: {
              type: 'string',
              description: 'The path to the directory to list'
            }
          },
          required: ['path']
        }
      },
      {
        name: 'execute_command',
        description:
          'Execute a shell command and return its output. Use this to run tests, build scripts, git commands, etc.',
        input_schema: {
          type: 'object',
          properties: {
            command: {
              type: 'string',
              description: 'The command to execute'
            },
            args: {
              type: 'array',
              items: { type: 'string' },
              description: 'Command arguments as an array'
            }
          },
          required: ['command', 'args']
        }
      }
    ];

    let finalResponse = '';
    const maxIterations = 30;
    let iteration = 0;

    try {
      while (iteration < maxIterations) {
        iteration++;
        console.log(`\n🔄 Iteration ${iteration}`);

        const response = await this.anthropic.messages.create({
          model: 'claude-sonnet-4-5-20250929',
          max_tokens: 8096,
          messages,
          tools
        });

        console.log(`📝 Stop reason: ${response.stop_reason}`);

        // Process response content
        for (const block of response.content) {
          if (block.type === 'text') {
            console.log(`💭 Claude: ${block.text.substring(0, 200)}...`);
            finalResponse += block.text + '\n';
          } else if (block.type === 'tool_use') {
            console.log(`🔧 Tool: ${block.name}(${JSON.stringify(block.input).substring(0, 100)}...)`);

            // Execute the tool
            const toolResult = await this.executeTool(
              block.name,
              block.input as any,
              workingDirectory
            );

            console.log(`✅ Tool result: ${typeof toolResult === 'string' ? toolResult.substring(0, 100) : JSON.stringify(toolResult).substring(0, 100)}...`);

            // Add assistant message with tool use
            messages.push({
              role: 'assistant',
              content: response.content
            });

            // Add tool result
            messages.push({
              role: 'user',
              content: [
                {
                  type: 'tool_result',
                  tool_use_id: block.id,
                  content: JSON.stringify(toolResult)
                }
              ]
            });
          }
        }

        // If we're done, break
        if (response.stop_reason === 'end_turn') {
          console.log('\n✨ Directive completed');
          break;
        }

        // If stop_reason is tool_use, we need to continue the loop
        if (response.stop_reason !== 'tool_use') {
          break;
        }
      }

      if (iteration >= maxIterations) {
        console.warn('⚠️  Max iterations reached');
      }

      return finalResponse.trim() || 'Task completed successfully';
    } catch (error) {
      console.error('❌ Error processing directive:', error);
      throw error;
    }
  }

  private async executeTool(
    name: string,
    input: any,
    workingDirectory?: string
  ): Promise<any> {
    switch (name) {
      case 'read_file':
        try {
          const content = await this.shipClient.readFile(input.path);
          return { success: true, content };
        } catch (error: any) {
          return { success: false, error: error.message };
        }

      case 'write_file':
        try {
          // Use ship client to write file via API
          await this.executeCommand(
            'sh',
            [
              '-c',
              `cat > "${input.path}" << 'TOREN_EOF'\n${input.content}\nTOREN_EOF`
            ],
            workingDirectory
          );
          return { success: true, path: input.path };
        } catch (error: any) {
          return { success: false, error: error.message };
        }

      case 'list_directory':
        try {
          const output = await this.executeCommand(
            'ls',
            ['-la', input.path],
            workingDirectory
          );
          return { success: true, output };
        } catch (error: any) {
          return { success: false, error: error.message };
        }

      case 'execute_command':
        try {
          const output = await this.executeCommand(
            input.command,
            input.args,
            workingDirectory
          );
          return { success: true, output };
        } catch (error: any) {
          return { success: false, error: error.message };
        }

      default:
        return { success: false, error: `Unknown tool: ${name}` };
    }
  }

  async readFile(path: string): Promise<string> {
    return this.shipClient.readFile(path);
  }

  async executeCommand(
    command: string,
    args: string[],
    cwd?: string
  ): Promise<string> {
    const outputs: string[] = [];

    const stream = await this.shipClient.executeCommand(command, args, cwd);

    for await (const output of stream) {
      switch (output.type) {
        case 'Stdout':
          if (output.line) outputs.push(output.line);
          break;
        case 'Stderr':
          if (output.line) outputs.push(`[stderr] ${output.line}`);
          break;
        case 'Exit':
          console.log(`Command exited with code ${output.code}`);
          break;
        case 'Error':
          throw new Error(output.message);
      }
    }

    return outputs.join('\n');
  }

  isRunning(): boolean {
    return this.running;
  }
}
