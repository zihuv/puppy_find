function puppyFind() {
    return {
        form: {
            model_path: '',
            asset_dir: ''
        },
        runtimeStatus: {
            snapshot: null
        },
        query: '',
        results: [],
        indexStatus: {
            running: false,
            indexed: 0,
            total: 0,
            processed: 0,
            current_file: null,
            error: null
        },
        saving: false,
        searching: false,
        message: '',
        error: '',
        indexError: '',
        pollHandle: null,
        showSettings: false,

        async init() {
            await this.loadSettings();
            await this.fetchIndexStatus();
            await this.fetchRuntimeStatus();
            this.pollHandle = window.setInterval(() => {
                this.fetchIndexStatus();
            }, 1000);
        },

        async loadSettings() {
            try {
                const response = await fetch('/api/settings');
                const data = await this.parseJson(response);
                this.form.model_path = data.model_path || '';
                this.form.asset_dir = data.asset_dir || '';
            } catch (error) {
                this.error = error.message;
            }
        },

        openSettings() {
            this.showSettings = true;
        },

        closeSettings() {
            if (this.saving) {
                return;
            }
            this.showSettings = false;
        },

        async saveSettings(silent = false) {
            if (!this.form.model_path || !this.form.asset_dir) {
                this.error = '请先填写 MODEL_PATH 和素材目录';
                return false;
            }

            this.saving = true;
            if (!silent) {
                this.message = '';
                this.error = '';
            }

            try {
                const response = await fetch('/api/settings', {
                    method: 'POST',
                    headers: {
                        'Content-Type': 'application/json'
                    },
                    body: JSON.stringify(this.form)
                });
                const data = await this.parseJson(response);
                if (!response.ok) {
                    throw new Error(data.error || '保存配置失败');
                }
                this.form.model_path = data.model_path;
                this.form.asset_dir = data.asset_dir;
                const messages = [];
                if (data.index_cleared) {
                    this.results = [];
                    this.indexStatus = {
                        ...this.indexStatus,
                        indexed: 0,
                        total: 0,
                        processed: 0,
                        current_file: null,
                        error: null
                    };
                    this.indexError = '';
                    messages.push('配置已保存，索引上下文已重置，请重新建索引。');
                } else if (!silent) {
                    messages.push('配置已保存。');
                }
                if (messages.length) {
                    this.message = messages.join(' ');
                }
                await this.fetchRuntimeStatus();
                return true;
            } catch (error) {
                this.error = error.message;
                return false;
            } finally {
                this.saving = false;
            }
        },

        async startIndex() {
            const saved = await this.saveSettings(true);
            if (!saved) {
                return;
            }

            this.message = '';
            this.error = '';
            this.indexError = '';

            try {
                const response = await fetch('/api/index', {
                    method: 'POST'
                });
                const data = await this.parseJson(response);
                if (!response.ok) {
                    throw new Error(data.error || data.message || '启动索引失败');
                }
                this.message = data.message || '索引任务已启动。';
                await this.fetchIndexStatus();
                await this.fetchRuntimeStatus();
            } catch (error) {
                this.error = error.message;
            }
        },

        async fetchIndexStatus() {
            try {
                const response = await fetch('/api/index/status');
                const data = await this.parseJson(response);
                this.indexStatus = data;
                this.indexError = data.error || '';
            } catch (error) {
                this.error = error.message;
            }
        },

        async fetchRuntimeStatus() {
            try {
                const response = await fetch('/api/runtime');
                const data = await this.parseJson(response);
                this.runtimeStatus.snapshot = data.snapshot || null;
            } catch (error) {
                this.runtimeStatus.snapshot = null;
            }
        },

        async openPath(path) {
            if (!path || !path.trim()) {
                this.error = '请先填写路径';
                return;
            }

            this.error = '';

            try {
                const response = await fetch('/api/open-path', {
                    method: 'POST',
                    headers: {
                        'Content-Type': 'application/json'
                    },
                    body: JSON.stringify({
                        path
                    })
                });
                const data = await this.parseJson(response);
                if (!response.ok) {
                    throw new Error(data.error || '打开路径失败');
                }
                this.message = data.message || '已打开路径';
            } catch (error) {
                this.error = error.message;
            }
        },

        async chooseDirectory(field) {
            const currentPath = this.form[field] || '';
            this.error = '';

            try {
                const response = await fetch('/api/pick-directory', {
                    method: 'POST',
                    headers: {
                        'Content-Type': 'application/json'
                    },
                    body: JSON.stringify({
                        path: currentPath
                    })
                });
                const data = await this.parseJson(response);
                if (!response.ok) {
                    throw new Error(data.error || '选择目录失败');
                }
                if (data.canceled) {
                    return;
                }
                this.form[field] = data.path || '';
            } catch (error) {
                this.error = error.message;
            }
        },

        async search() {
            if (!this.query) {
                this.error = '请输入搜索文本';
                return;
            }

            this.searching = true;
            this.error = '';
            this.message = '';

            try {
                const response = await fetch('/api/search', {
                    method: 'POST',
                    headers: {
                        'Content-Type': 'application/json'
                    },
                    body: JSON.stringify({
                        query: this.query,
                        limit: 60
                    })
                });
                const data = await this.parseJson(response);
                if (!response.ok) {
                    throw new Error(data.error || '搜索失败');
                }
                this.results = data.items || [];
                if (!this.results.length) {
                    this.message = '没有找到相似图片。';
                }
            } catch (error) {
                this.error = error.message;
            } finally {
                await this.fetchRuntimeStatus();
                this.searching = false;
            }
        },

        scanProgressText() {
            if (this.indexStatus.running || this.indexStatus.total > 0) {
                return `${this.indexStatus.processed}/${this.indexStatus.total}`;
            }
            if (this.indexStatus.indexed > 0) {
                return `${this.indexStatus.processed || this.indexStatus.indexed}/${this.indexStatus.total || this.indexStatus.indexed}`;
            }
            return '未开始';
        },

        runtimeDeviceText() {
            const summary = this.runtimeStatus.snapshot?.summary;
            const provider = summary?.effective_provider;
            if (provider) {
                return this.isGpuProvider(provider) ? 'gpu' : 'cpu';
            }

            if (summary?.mode === 'cpu_only') {
                return 'cpu';
            }

            if (summary?.mode === 'gpu_enabled' || summary?.mode === 'mixed') {
                return 'gpu';
            }

            return '';
        },

        isGpuProvider(provider) {
            return provider === 'cuda'
                || provider === 'direct_ml'
                || provider === 'core_ml'
                || provider === 'tensor_rt';
        },

        async parseJson(response) {
            const text = await response.text();
            if (!text) {
                return {};
            }
            try {
                return JSON.parse(text);
            } catch (error) {
                throw new Error('服务端返回了无效响应');
            }
        }
    };
}
