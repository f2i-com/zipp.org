class _Logger:
    def warning(self, *args, **kwargs):
        pass

    def warning_once(self, *args, **kwargs):
        pass

    def info(self, *args, **kwargs):
        pass

    def debug(self, *args, **kwargs):
        pass


def get_logger(name=None):
    return _Logger()
