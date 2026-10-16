"""Extract, format and print information about Python stack traces.

The standard library's `traceback` over the traceback records the engine
builds from the frames an exception passed (`e.__traceback__`). Frames carry
no column positions here, so a frame prints its source line without the
caret line CPython draws under the failing expression.
"""
import sys
import linecache

__all__ = ['extract_stack', 'extract_tb', 'format_exception',
           'format_exception_only', 'format_list', 'format_stack',
           'format_tb', 'print_exc', 'format_exc', 'print_exception',
           'print_last', 'print_stack', 'print_tb', 'clear_frames',
           'FrameSummary', 'StackSummary', 'TracebackException',
           'walk_stack', 'walk_tb']


def print_list(extracted_list, file=None):
    """Print the list of tuples as returned by extract_tb() or
    extract_stack() as a formatted stack trace to the given file."""
    if file is None:
        file = sys.stderr
    for item in StackSummary.from_list(extracted_list).format():
        print(item, file=file, end="")


def format_list(extracted_list):
    """Format a list of FrameSummary objects (or old-style tuples) for
    printing."""
    return StackSummary.from_list(extracted_list).format()


def print_tb(tb, limit=None, file=None):
    """Print up to 'limit' stack trace entries from the traceback 'tb'."""
    print_list(extract_tb(tb, limit=limit), file=file)


def format_tb(tb, limit=None):
    """A shorthand for 'format_list(extract_tb(tb, limit))'."""
    return extract_tb(tb, limit=limit).format()


def extract_tb(tb, limit=None):
    """Return a StackSummary of the entries of the traceback 'tb'."""
    return StackSummary.extract(walk_tb(tb), limit=limit)


_cause_message = (
    "\nThe above exception was the direct cause "
    "of the following exception:\n\n")

_context_message = (
    "\nDuring handling of the above exception, "
    "another exception occurred:\n\n")


class _Sentinel:
    def __repr__(self):
        return "<implicit>"


_sentinel = _Sentinel()


def _parse_value_tb(exc, value, tb):
    if (value is _sentinel) != (tb is _sentinel):
        raise ValueError("Both or neither of value and tb must be given")
    if value is tb is _sentinel:
        if exc is not None:
            if isinstance(exc, BaseException):
                return exc, exc.__traceback__
            raise TypeError(f'Exception expected for value, '
                            f'{type(exc).__name__} found')
        else:
            return None, None
    return value, tb


def print_exception(exc, /, value=_sentinel, tb=_sentinel, limit=None,
                    file=None, chain=True, **kwargs):
    """Print exception up to 'limit' stack trace entries from 'tb' to
    'file', with its chained exceptions when 'chain' is true."""
    value, tb = _parse_value_tb(exc, value, tb)
    te = TracebackException(type(value), value, tb, limit=limit, compact=True)
    te.print(file=file, chain=chain)


def format_exception(exc, /, value=_sentinel, tb=_sentinel, limit=None,
                     chain=True, **kwargs):
    """Format a stack trace and the exception information: a list of
    strings, each ending in a newline, that print_exception() prints."""
    value, tb = _parse_value_tb(exc, value, tb)
    te = TracebackException(type(value), value, tb, limit=limit, compact=True)
    return list(te.format(chain=chain))


def format_exception_only(exc, /, value=_sentinel, *, show_group=False, **kwargs):
    """Format the exception part of a traceback: its message (for a
    SyntaxError, the lines locating it first), then its __notes__."""
    if value is _sentinel:
        value = exc
    te = TracebackException(type(value), value, None, compact=True)
    return list(te.format_exception_only(show_group=show_group))


def _format_final_exc_line(etype, value, *, insert_final_newline=True, colorize=False):
    valuestr = _safe_string(value, 'exception')
    end_char = "\n" if insert_final_newline else ""
    if value is None or not valuestr:
        return f"{etype}{end_char}"
    return f"{etype}: {valuestr}{end_char}"


def _safe_string(value, what, func=str):
    try:
        return func(value)
    except:
        return f'<{what} {func.__name__}() failed>'


def print_exc(limit=None, file=None, chain=True):
    """Print the exception being handled, as print_exception() does."""
    print_exception(sys.exception(), limit=limit, file=file, chain=chain)


def format_exc(limit=None, chain=True):
    """Like print_exc() but return a string."""
    return "".join(format_exception(sys.exception(), limit=limit, chain=chain))


def print_last(limit=None, file=None, chain=True):
    """Print the last exception that left the interactive prompt."""
    if not hasattr(sys, "last_exc") and not hasattr(sys, "last_type"):
        raise ValueError("no last exception")
    if hasattr(sys, "last_exc"):
        print_exception(sys.last_exc, limit=limit, file=file, chain=chain)
    else:
        print_exception(sys.last_type, sys.last_value, sys.last_traceback,
                        limit=limit, file=file, chain=chain)


def print_stack(f=None, limit=None, file=None):
    """Print a stack trace from its invocation point (no frames are
    available here: an empty stack)."""
    print_list(extract_stack(f, limit=limit), file=file)


def format_stack(f=None, limit=None):
    """Shorthand for 'format_list(extract_stack(f, limit))'."""
    return format_list(extract_stack(f, limit=limit))


def extract_stack(f=None, limit=None):
    """The stack from frame 'f' outwards, oldest first. Running frames are
    not objects here, so only a frame from a traceback has a stack."""
    stack = StackSummary.extract(walk_stack(f), limit=limit)
    stack.reverse()
    return stack


def clear_frames(tb):
    """Clear all references to local variables in the frames of a
    traceback (none are kept)."""
    pass


class FrameSummary:
    """Information about a single frame from a traceback: its filename,
    lineno, name and source line, and the frame's locals when captured."""

    def __init__(self, filename, lineno, name, *, lookup_line=True,
                 locals=None, line=None,
                 end_lineno=None, colno=None, end_colno=None, **kwargs):
        self.filename = filename
        self.lineno = lineno
        self.end_lineno = lineno if end_lineno is None else end_lineno
        self.colno = colno
        self.end_colno = end_colno
        self.name = name
        self._lines = line
        if lookup_line:
            self.line
        self.locals = {k: _safe_string(v, 'local', func=repr)
                       for k, v in locals.items()} if locals else None

    def __eq__(self, other):
        if isinstance(other, FrameSummary):
            return (self.filename == other.filename and
                    self.lineno == other.lineno and
                    self.name == other.name and
                    self.locals == other.locals)
        if isinstance(other, tuple):
            return (self.filename, self.lineno, self.name, self.line) == other
        return NotImplemented

    def __getitem__(self, pos):
        return (self.filename, self.lineno, self.name, self.line)[pos]

    def __iter__(self):
        return iter([self.filename, self.lineno, self.name, self.line])

    def __repr__(self):
        return "<FrameSummary file {filename}, line {lineno} in {name}>".format(
            filename=self.filename, lineno=self.lineno, name=self.name)

    def __len__(self):
        return 4

    def _set_lines(self):
        if self._lines is None and self.lineno is not None and self.end_lineno is not None:
            lines = []
            for lineno in range(self.lineno, self.end_lineno + 1):
                lines.append(linecache.getline(self.filename, lineno).rstrip())
            self._lines = "\n".join(lines) + "\n"

    @property
    def line(self):
        self._set_lines()
        if self._lines is None:
            return None
        return self._lines.partition("\n")[0].strip()


def walk_stack(f):
    """Walk a stack following f.f_back, yielding each frame and its line."""
    while f is not None:
        yield f, f.f_lineno
        f = f.f_back


def walk_tb(tb):
    """Walk a traceback following tb.tb_next, yielding each frame and its
    line."""
    while tb is not None:
        yield tb.tb_frame, tb.tb_lineno
        tb = tb.tb_next


_RECURSIVE_CUTOFF = 3


class StackSummary(list):
    """A list of FrameSummary objects, representing a stack of frames."""

    @classmethod
    def extract(klass, frame_gen, *, limit=None, lookup_lines=True,
                capture_locals=False):
        """Create a StackSummary from (frame, lineno) pairs."""
        if limit is None:
            limit = getattr(sys, 'tracebacklimit', None)
            if limit is not None and limit < 0:
                limit = 0
        frames = list(frame_gen)
        if limit is not None:
            frames = frames[:limit] if limit >= 0 else frames[len(frames) + limit:] if -limit < len(frames) else frames
        result = klass()
        for f, lineno in frames:
            co = f.f_code
            f_locals = f.f_locals if capture_locals else None
            result.append(FrameSummary(co.co_filename, lineno, co.co_name,
                                       lookup_line=False, locals=f_locals))
        if lookup_lines:
            for f in result:
                f.line
        return result

    @classmethod
    def from_list(klass, a_list):
        """Create a StackSummary from FrameSummary objects or old-style
        (filename, lineno, name, line) tuples."""
        result = StackSummary()
        for frame in a_list:
            if isinstance(frame, FrameSummary):
                result.append(frame)
            else:
                filename, lineno, name, line = frame
                result.append(FrameSummary(filename, lineno, name, line=line))
        return result

    def format_frame_summary(self, frame_summary, **kwargs):
        """The lines for a single FrameSummary: its File line, then its
        source line."""
        row = []
        filename = frame_summary.filename
        if filename.startswith("<stdin-") and filename.endswith('>'):
            filename = "<stdin>"
        row.append('  File "{}", line {}, in {}\n'.format(
            filename, frame_summary.lineno, frame_summary.name))
        line = frame_summary.line
        if line:
            row.append('    {}\n'.format(line))
        if frame_summary.locals:
            for name, value in sorted(frame_summary.locals.items()):
                row.append('    {name} = {value}\n'.format(name=name, value=value))
        return ''.join(row)

    def format(self, **kwargs):
        """Format the stack ready for printing: one string per frame, a long
        run of the same frame and line shown three times and then counted."""
        result = []
        last_file = None
        last_line = None
        last_name = None
        count = 0
        for frame_summary in self:
            formatted_frame = self.format_frame_summary(frame_summary)
            if formatted_frame is None:
                continue
            if (last_file is None or last_file != frame_summary.filename or
                    last_line is None or last_line != frame_summary.lineno or
                    last_name is None or last_name != frame_summary.name):
                if count > _RECURSIVE_CUTOFF:
                    count -= _RECURSIVE_CUTOFF
                    result.append(
                        f'  [Previous line repeated {count} more '
                        f'time{"s" if count > 1 else ""}]\n'
                    )
                last_file = frame_summary.filename
                last_line = frame_summary.lineno
                last_name = frame_summary.name
                count = 0
            count += 1
            if count > _RECURSIVE_CUTOFF:
                continue
            result.append(formatted_frame)
        if count > _RECURSIVE_CUTOFF:
            count -= _RECURSIVE_CUTOFF
            result.append(
                f'  [Previous line repeated {count} more '
                f'time{"s" if count > 1 else ""}]\n'
            )
        return result


def _compute_suggestion_error(exc_value, tb, wrong_name):
    if wrong_name is None or not isinstance(wrong_name, str):
        return None
    if isinstance(exc_value, AttributeError):
        obj = exc_value.obj
        try:
            d = sorted([x for x in dir(obj) if isinstance(x, str)])
            hide_underscored = (wrong_name[:1] != '_')
            if hide_underscored and tb is not None:
                while tb.tb_next is not None:
                    tb = tb.tb_next
                frame = tb.tb_frame
                if 'self' in frame.f_locals and frame.f_locals['self'] is obj:
                    hide_underscored = False
            if hide_underscored:
                d = [x for x in d if x[:1] != '_']
        except Exception:
            return None
    elif isinstance(exc_value, ImportError):
        try:
            mod = __import__(exc_value.name)
            d = sorted([x for x in dir(mod) if isinstance(x, str)])
            if wrong_name[:1] != '_':
                d = [x for x in d if x[:1] != '_']
        except Exception:
            return None
    else:
        if tb is None:
            return None
        while tb.tb_next is not None:
            tb = tb.tb_next
        frame = tb.tb_frame
        d = (
            list(frame.f_locals)
            + list(frame.f_globals)
            + list(frame.f_builtins)
        )
        d = [x for x in d if isinstance(x, str)]
        if 'self' in frame.f_locals:
            self = frame.f_locals['self']
            try:
                has_wrong_name = hasattr(self, wrong_name)
            except Exception:
                has_wrong_name = False
            if has_wrong_name:
                return f"self.{wrong_name}"
    import _suggestions
    return _suggestions._generate_suggestions(d, wrong_name)


class TracebackException:
    """An exception ready for rendering: its stack, type, message, notes,
    and its __cause__ / __context__ as TracebackExceptions."""

    def __init__(self, exc_type, exc_value, exc_traceback, *, limit=None,
                 lookup_lines=True, capture_locals=False, compact=False,
                 max_group_width=15, max_group_depth=10, save_exc_type=True, _seen=None):
        is_recursive_call = _seen is not None
        if _seen is None:
            _seen = set()
        _seen.add(id(exc_value))
        self.max_group_width = max_group_width
        self.max_group_depth = max_group_depth
        self.stack = StackSummary.extract(
            walk_tb(exc_traceback), limit=limit, lookup_lines=lookup_lines,
            capture_locals=capture_locals)
        self._exc_type = exc_type if save_exc_type else None
        self._str = _safe_string(exc_value, 'exception')
        try:
            self.__notes__ = getattr(exc_value, '__notes__', None)
        except Exception as e:
            self.__notes__ = [
                f'Ignored error getting __notes__: {_safe_string(e, "__notes__", repr)}']
        self._is_syntax_error = False
        self._have_exc_type = exc_type is not None
        if exc_type is not None:
            self.exc_type_qualname = exc_type.__qualname__
            self.exc_type_module = exc_type.__module__
        else:
            self.exc_type_qualname = None
            self.exc_type_module = None
        if exc_type and issubclass(exc_type, SyntaxError):
            self.filename = getattr(exc_value, 'filename', None)
            lno = getattr(exc_value, 'lineno', None)
            self.lineno = str(lno) if lno is not None else None
            end_lno = getattr(exc_value, 'end_lineno', None)
            self.end_lineno = str(end_lno) if end_lno is not None else None
            self.text = getattr(exc_value, 'text', None)
            self.offset = getattr(exc_value, 'offset', None)
            self.end_offset = getattr(exc_value, 'end_offset', None)
            self.msg = getattr(exc_value, 'msg', None)
            self._is_syntax_error = True
        elif exc_type and issubclass(exc_type, ImportError) and                 getattr(exc_value, "name_from", None) is not None:
            wrong_name = getattr(exc_value, "name_from", None)
            suggestion = _compute_suggestion_error(exc_value, exc_traceback, wrong_name)
            if suggestion:
                self._str += f". Did you mean: '{suggestion}'?"
        elif exc_type and issubclass(exc_type, (NameError, AttributeError)) and                 getattr(exc_value, "name", None) is not None:
            wrong_name = getattr(exc_value, "name", None)
            suggestion = _compute_suggestion_error(exc_value, exc_traceback, wrong_name)
            if suggestion:
                self._str += f". Did you mean: '{suggestion}'?"
            if issubclass(exc_type, NameError):
                if wrong_name is not None and wrong_name in sys.stdlib_module_names:
                    if suggestion:
                        self._str += f" Or did you forget to import '{wrong_name}'?"
                    else:
                        self._str += f". Did you forget to import '{wrong_name}'?"
        self.__suppress_context__ = \
            exc_value.__suppress_context__ if exc_value is not None else False
        self.exceptions = None
        self.__cause__ = None
        self.__context__ = None
        if not is_recursive_call:
            queue = [(self, exc_value)]
            while queue:
                te, e = queue.pop()
                if (e is not None and e.__cause__ is not None
                        and id(e.__cause__) not in _seen):
                    cause = TracebackException(
                        type(e.__cause__), e.__cause__, e.__cause__.__traceback__,
                        limit=limit, lookup_lines=lookup_lines,
                        capture_locals=capture_locals, _seen=_seen)
                else:
                    cause = None
                if compact:
                    need_context = (cause is None and e is not None and
                                    not e.__suppress_context__)
                else:
                    need_context = True
                if (e is not None and e.__context__ is not None
                        and need_context and id(e.__context__) not in _seen):
                    context = TracebackException(
                        type(e.__context__), e.__context__, e.__context__.__traceback__,
                        limit=limit, lookup_lines=lookup_lines,
                        capture_locals=capture_locals, _seen=_seen)
                else:
                    context = None
                te.__cause__ = cause
                te.__context__ = context
                if cause:
                    queue.append((te.__cause__, e.__cause__))
                if context:
                    queue.append((te.__context__, e.__context__))

    @classmethod
    def from_exception(cls, exc, *args, **kwargs):
        """Create a TracebackException from an exception."""
        return cls(type(exc), exc, exc.__traceback__, *args, **kwargs)

    @property
    def exc_type(self):
        return self._exc_type

    @property
    def exc_type_str(self):
        if not self._have_exc_type:
            return None
        stype = self.exc_type_qualname
        smod = self.exc_type_module
        if smod not in ("__main__", "builtins"):
            if not isinstance(smod, str):
                smod = "<unknown>"
            stype = smod + '.' + stype
        return stype

    def __eq__(self, other):
        if isinstance(other, TracebackException):
            return self.__dict__ == other.__dict__
        return NotImplemented

    def __str__(self):
        return self._str

    def format_exception_only(self, *, show_group=False, _depth=0, **kwargs):
        """The exception part of the traceback: its message (for a
        SyntaxError, the lines locating it first), then its __notes__."""
        indent = 3 * _depth * ' '
        if not self._have_exc_type:
            yield indent + _format_final_exc_line(None, self._str)
            return
        stype = self.exc_type_str
        if not self._is_syntax_error:
            yield _format_final_exc_line(stype, self._str)
        else:
            for line in self._format_syntax_error(stype):
                yield indent + line
        notes = self.__notes__
        if isinstance(notes, (list, tuple)):
            for note in notes:
                note = _safe_string(note, 'note')
                for line in note.split('\n'):
                    yield indent + line + '\n'
        elif notes is not None:
            yield indent + "{}\n".format(_safe_string(notes, '__notes__', func=repr))

    def _format_syntax_error(self, stype, **kwargs):
        filename_suffix = ''
        if self.lineno is not None:
            yield '  File "{}", line {}\n'.format(
                self.filename or "<string>", self.lineno)
        elif self.filename is not None:
            filename_suffix = ' ({})'.format(self.filename)
        text = self.text
        if isinstance(text, str):
            rtext = text.rstrip('\n')
            ltext = rtext.lstrip(' \n\f')
            spaces = len(rtext) - len(ltext)
            if self.offset is None:
                yield '    {}\n'.format(ltext)
            elif isinstance(self.offset, int):
                offset = self.offset
                if self.lineno == self.end_lineno:
                    end_offset = (self.end_offset
                                  if isinstance(self.end_offset, int) and self.end_offset != 0
                                  else offset)
                else:
                    end_offset = len(rtext) + 1
                if self.text and offset > len(self.text):
                    offset = len(rtext) + 1
                if self.text and end_offset > len(self.text):
                    end_offset = len(rtext) + 1
                if offset >= end_offset or end_offset < 0:
                    end_offset = offset + 1
                colno = offset - 1 - spaces
                end_colno = end_offset - 1 - spaces
                if colno >= 0:
                    caretspace = ''.join((c if c.isspace() else ' ') for c in ltext[:colno])
                    yield '    {}\n'.format(ltext)
                    yield '    {}{}\n'.format(caretspace, '^' * (end_colno - colno))
                else:
                    yield '    {}\n'.format(ltext)
        msg = self.msg or "<no detail available>"
        yield "{}: {}{}\n".format(stype, msg, filename_suffix)

    def format(self, *, chain=True, _ctx=None, **kwargs):
        """Format the exception: its chained exceptions first (when 'chain'
        is true), each with its traceback; the last string is the message
        naming this exception."""
        output = []
        exc = self
        if chain:
            while exc:
                if exc.__cause__ is not None:
                    chained_msg = _cause_message
                    chained_exc = exc.__cause__
                elif exc.__context__ is not None and not exc.__suppress_context__:
                    chained_msg = _context_message
                    chained_exc = exc.__context__
                else:
                    chained_msg = None
                    chained_exc = None
                output.append((chained_msg, exc))
                exc = chained_exc
        else:
            output.append((None, exc))
        for msg, exc in reversed(output):
            if msg is not None:
                yield msg
            if exc.stack:
                yield 'Traceback (most recent call last):\n'
                yield from exc.stack.format()
            yield from exc.format_exception_only()

    def print(self, *, file=None, chain=True, **kwargs):
        """Print the result of self.format(chain=chain) to 'file'."""
        if file is None:
            file = sys.stderr
        for line in self.format(chain=chain):
            print(line, file=file, end="")
