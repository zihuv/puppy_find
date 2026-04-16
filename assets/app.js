function puppyFind() {
    return {
        form: {
            db_path: '',
            model_path: '',
            host: '127.0.0.1',
            port: 3000,
            asset_dir: ''
        },
        query: '',
        results: [],
        indexStatus: {
            running: false,
            total: 0,
            processed: 0,
            current_file: null,
            error: null
        },
        saving: false,
        searching: false,
        message: '',
        error: '',
        pollHandle: null,
        showSettings: false,

        async init() {
            await this.loadSettings();
            await this.fetchIndexStatus();
            this.pollHandle = window.setInterval(() => {
                this.fetchIndexStatus();
            }, 1000);
        },

        async loadSettings() {
            try {
                const response = await fetch('/api/settings');
                const data = await this.parseJson(response);
                this.form.db_path = data.db_path || '';
                this.form.model_path = data.model_path || '';
                this.form.host = data.host || '127.0.0.1';
                this.form.port = Number(data.port || 3000);
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
            if (!this.form.db_path || !this.form.model_path || !this.form.host || !this.form.port || !this.form.asset_dir) {
                this.error = '请先填写数据库文件位置、MODEL_PATH、HOST、PORT 和素材目录';
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
                this.form.db_path = data.db_path;
                this.form.model_path = data.model_path;
                this.form.host = data.host;
                this.form.port = Number(data.port || 3000);
                this.form.asset_dir = data.asset_dir;
                const messages = [];
                if (data.index_cleared) {
                    this.results = [];
                    messages.push('配置已保存，索引上下文已重置，请重新建索引。');
                } else if (!silent) {
                    messages.push('配置已保存。');
                }
                if (data.restart_required) {
                    messages.push('HOST/PORT 已写入 .env，重启程序后生效。');
                }
                if (messages.length) {
                    this.message = messages.join(' ');
                }
                if (!silent) {
                    this.showSettings = false;
                }
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

            try {
                const response = await fetch('/api/index', {
                    method: 'POST'
                });
                const data = await this.parseJson(response);
                if (!response.ok) {
                    throw new Error(data.error || data.message || '启动索引失败');
                }
                this.message = data.message;
                this.showSettings = false;
                await this.fetchIndexStatus();
            } catch (error) {
                this.error = error.message;
            }
        },

        async fetchIndexStatus() {
            try {
                const response = await fetch('/api/index/status');
                const data = await this.parseJson(response);
                this.indexStatus = data;
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
                this.searching = false;
            }
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
