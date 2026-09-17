def deprecate_kwarg(name, new_name=None, version=None, **extra):
    """Accept the old keyword and pass it along under the new one."""
    def decorate(function):
        def wrapper(*args, **kwargs):
            if new_name is not None and name in kwargs:
                value = kwargs.pop(name)
                if kwargs.get(new_name) is None:
                    kwargs[new_name] = value
            else:
                kwargs.pop(name, None)
            return function(*args, **kwargs)
        return wrapper
    return decorate
